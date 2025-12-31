#![no_std]

///! ------------------------------------
///! Buzzer Music for Rust using Embassy.
///! ------------------------------------
///! Create music using one or more piezo buzzers with Rust!
///!
///! https://github.com/SomeRanDev/buzzer_music.rs/blob/main/LICENSE
///!
///! Heavily based on https://github.com/james1236/buzzer_music
///! https://github.com/james1236/buzzer_music/blob/main/LICENSE

/// Creates an instance of [`buzzer_music::Song`] using the `onlinesequencer.net` format.
/// This parses the content at compile-time and produces a packed version of the song.
///
/// ```rust
/// const MYSTERY_SONG: buzzer_music::Song = declare_song!("0 D5 1 11;2 D5 1 11;4 D6 1 11;8 A5 1 11;14 G#5 1 11;18 G5 1 11;22 F5 1 11;26 D5 1 11;28 F5 1 11;30 G5 1 11;0 D4 1 15;2 D4 1 15;4 D5 1 15;8 A4 1 15;14 G#4 1 15;18 G4 1 15;22 F4 1 15;26 D4 1 15;28 F4 1 15;30 G4 1 15;0 D4 1.75 14;2 D4 1.75 14;4 D5 1.75 14;8 A4 1.75 14;14 G#4 1.75 14;18 G4 1.75 14;22 F4 1.75 14;26 D4 1.75 14;28 F4 1.75 14;30 G4 1.75 14");
/// ```
pub use buzzer_music_macros::declare_song;

/// Represents a song.
pub struct Song {
	pub notes: &'static [Option<&'static [NoteAndDuration]>],
	pub end: u16,
}

/// Represents a frequency and its duration.
#[derive(Clone, Copy)]
pub struct NoteAndDuration {
	pub frequency: u16,
	pub duration: u16,
}

/// The fractional clock divider used in PWM.
/// Based on https://pico.implrust.com/buzzer/play-songs/code.html.
const PWM_DIV_INT: u8 = 64;

/// Generates the `top` value used in PWM.
/// From https://pico.implrust.com/buzzer/play-songs/code.html.
const fn get_top(freq: f64, div_int: u8) -> u16 {
	assert!(div_int != 0, "Divider must not be 0");

	let result = 150_000_000. / (freq * div_int as f64);

	assert!(result >= 1.0, "Frequency too high");
	assert!(
		result <= 65535.0,
		"Frequency too low: TOP exceeds 65534 max"
	);

	result as u16 - 1
}

/// Plays a [`buzzer_music::Song`].
///
/// ```rust
/// let p = embassy_rp::init(Default::default());
///
/// // Create Pwm instance.
/// let mut buzzer = embassy_rp::pwm::Pwm::new_output_b(p.PWM_SLICE7, p.PIN_15, embassy_rp::pwm::Config::default());
///
/// // Pass song and Pwm to Player.
/// let player = buzzer_music::Player::new(&MYSTERY_SONG, true, 3, 100, [buzzer]);
///
/// // Update every 40ms.
/// loop {
/// 	player.tick();
/// 	embassy_time::Timer::after_millis(40).await;
/// }
/// ```
///
/// It can use one or more [`embassy_rp::pwm::Pwm`]s, but the count must be defined via `PWM_COUNT`.
///
/// The `MAX_SIMULTANEOUS_NOTES` dictates the maximum number of notes that can play simultamously since
/// the notes needs to be preemptively allocated on the stack via [`arrayvec::ArrayVec`].
pub struct Player<'a, const PWM_COUNT: usize, const MAX_SIMULTANEOUS_NOTES: usize> {
	song: &'a Song,
	looping: bool,
	ticks_per_beat: u16,
	duty: u16,
	pwms: [embassy_rp::pwm::Pwm<'a>; PWM_COUNT],

	paused: bool,
	timer: u16,
	beat_timer: u16,
	beat: i32,
	current_combined_note_index: usize,
	playing_notes: arrayvec::ArrayVec<NoteAndDuration, MAX_SIMULTANEOUS_NOTES>,
}

impl<'a, const PWM_COUNT: usize, const MAX_SIMULTANEOUS_NOTES: usize>
	Player<'a, PWM_COUNT, MAX_SIMULTANEOUS_NOTES>
{
	/// The constructor.
	///
	/// `song` is a reference to the `buzzer_music::Song` to play.
	/// `looping`, if true, will have the song start at the beginning once it ends.
	/// `ticks_per_beat` determines how many ticks must run before the next note is played.
	/// `duty` is the raw duty value assigned to the PWMs.
	/// `pwms` is an array of PWMs of length `PWM_COUNT`.
	pub fn new(
		song: &'a Song,
		looping: bool,
		ticks_per_beat: u16,
		duty: u16,
		pwms: [embassy_rp::pwm::Pwm<'a>; PWM_COUNT],
	) -> Self {
		Self {
			song,
			looping,
			ticks_per_beat,
			duty,
			pwms,

			paused: false,
			timer: 0,
			beat_timer: 0,
			beat: -1,
			current_combined_note_index: 0,
			playing_notes: arrayvec::ArrayVec::new(),
		}
	}

	/// Pauses the song. It can be resumed using [`resume`].
	/// This doesn't do anything if already paused.
	pub fn pause(&mut self) {
		if !self.paused {
			use embassy_rp::pwm::SetDutyCycle;
			for i in 0..PWM_COUNT {
				self.pwms[i].set_duty_cycle_fully_off().unwrap();
			}
			self.paused = true;
		}
	}

	/// Resumes after calling [`pause`].
	/// This doesn't do anything if not paused.
	pub fn resume(&mut self) {
		if self.paused {
			self.paused = false;
		}
	}

	/// Starts the song from the beginning.
	/// Will play if paused.
	pub fn restart(&mut self) {
		self.reset_internally();
		self.pause();
		self.resume();
	}

	/// Resets the song to the start.
	fn reset_internally(&mut self) {
		self.beat = -1;
		self.timer = 0;
	}

	/// Updates the player.
	/// This should be called every loop.
	///
	/// The `ticks_per_beat` provided in the constructor dictates how many `tick`s it takes to play the next note in sequence.
	/// So if you [`tick`] every 40ms with a tempo of `3`, the "real tempo" is 120ms.
	///
	/// Returns `false` if paused, `true` if successful!
	pub fn tick(&mut self) -> bool {
		if self.paused {
			return false;
		}

		// Increment that timer!
		self.timer += 1;
		self.beat_timer += 1;

		// Let's check if we're at the end of the song.
		// If so, go to the start of the song if `looping` is `true` (pause otherwise).
		if self.timer != 0 && (self.timer % (self.ticks_per_beat * self.song.end) == 0) {
			if !self.looping {
				self.pause();
				return false;
			}
			self.reset_internally();
		}

		// Once we're hit enough ticks, increment the beat.
		if self.beat_timer == self.ticks_per_beat {
			self.play_beat();
			self.beat_timer = 0;
		}

		// If we're playing multiple notes at the same time, cycle them through the buzzer.
		// Every tick the note should be updated unless we're playing one note.
		if self.playing_notes.len() > PWM_COUNT {
			if self.current_combined_note_index > (self.playing_notes.len() - PWM_COUNT) {
				self.current_combined_note_index = 0;
			}

			self.set_frequency_and_duty(
				PWM_COUNT - 1,
				self.playing_notes[self.current_combined_note_index + PWM_COUNT - 1].frequency,
				self.duty,
			);

			self.current_combined_note_index += 1;
		}

		true
	}

	fn play_beat(&mut self) {
		self.beat += 1;

		// Remove expired notes from playing list
		{
			let mut i = 0;
			while i < self.playing_notes.len() {
				self.playing_notes[i].duration -= 1;
				if self.playing_notes[i].duration <= 0 {
					self.playing_notes.remove(i);
				} else {
					i += 1;
				}
			}
		}

		// Add new notes and their durations to the playing list
		if self.beat < self.song.notes.len() as i32 {
			if let Some(notes) = &self.song.notes[self.beat as usize] {
				for note in *notes {
					self.playing_notes.push(*note);
				}
			}
		}

		// Only need to run these checks on beats
		{
			let mut i = 0;
			while i < PWM_COUNT {
				use embassy_rp::pwm::SetDutyCycle;

				if i >= self.playing_notes.len() {
					self.pwms[i].set_duty_cycle_fully_off().unwrap();
				} else {
					self.set_frequency_and_duty(i, self.playing_notes[i].frequency, self.duty);
				}

				i += 1;
			}
		}
	}

	/// Updates the `frequency` and `duty` of a PWM at index `pwm_index`.
	fn set_frequency_and_duty(&mut self, pwm_index: usize, frequency: u16, duty: u16) {
		use embassy_rp::pwm::SetDutyCycle;

		let pwm = &mut self.pwms[pwm_index];
		pwm.set_duty_cycle_fully_off().unwrap(); // `set_config` doesn't work unless this off??

		let mut pwm_config = embassy_rp::pwm::Config::default();
		pwm_config.top = get_top(frequency as f64, PWM_DIV_INT);
		pwm_config.divider = PWM_DIV_INT.into();
		pwm.set_config(&pwm_config);

		pwm.set_duty_cycle(duty).unwrap();
	}
}
