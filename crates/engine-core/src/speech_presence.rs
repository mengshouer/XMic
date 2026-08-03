/// Frames of look-back the detector keeps, at the fixed 10 ms frame size.
const WINDOW_FRAMES: usize = 300;
/// Qualifying frames required inside the window before speech is reported.
const REQUIRED_FRAMES: usize = 100;
/// Deliberately stricter than the VAD gate: the gate has to decide within milliseconds, this only
/// has to decide whether to show a notification, so it can afford to wait for certainty.
const PROBABILITY_THRESHOLD: f32 = 0.9;
/// Rejects near-silent frames whose probability happens to spike.
const LEVEL_FLOOR_DBFS: f32 = -45.0;

/// Reports that somebody is talking into a microphone whose audio is *not* being processed.
///
/// Impulsive noise is the failure mode that matters here — a loud keyboard peaks well above the
/// level floor and can push a single-frame probability high, so a frame count alone would misfire.
/// Requiring a full second of qualifying frames inside a three second window means only sustained
/// voice-like energy passes. The detector fires once and then stays quiet until [`Self::reset`].
#[derive(Debug)]
pub struct SpeechPresenceDetector {
    window: [bool; WINDOW_FRAMES],
    cursor: usize,
    qualifying: usize,
    fired: bool,
}

impl Default for SpeechPresenceDetector {
    fn default() -> Self {
        Self {
            window: [false; WINDOW_FRAMES],
            cursor: 0,
            qualifying: 0,
            fired: false,
        }
    }
}

impl SpeechPresenceDetector {
    /// Returns true on the single frame where sustained speech is confirmed.
    pub fn observe(&mut self, vad_probability: f32, peak_dbfs: f32) -> bool {
        let qualifies = vad_probability.is_finite()
            && vad_probability >= PROBABILITY_THRESHOLD
            && peak_dbfs.is_finite()
            && peak_dbfs >= LEVEL_FLOOR_DBFS;
        if self.window[self.cursor] {
            self.qualifying -= 1;
        }
        self.window[self.cursor] = qualifies;
        if qualifies {
            self.qualifying += 1;
        }
        self.cursor = (self.cursor + 1) % WINDOW_FRAMES;

        if self.fired || self.qualifying < REQUIRED_FRAMES {
            return false;
        }
        self.fired = true;
        true
    }

    pub fn fired(&self) -> bool {
        self.fired
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn speech_frame(detector: &mut SpeechPresenceDetector) -> bool {
        detector.observe(0.95, -20.0)
    }

    fn silent_frame(detector: &mut SpeechPresenceDetector) -> bool {
        detector.observe(0.02, -80.0)
    }

    #[test]
    fn sustained_speech_fires_once_after_a_full_second() {
        let mut detector = SpeechPresenceDetector::default();

        for _ in 0..(REQUIRED_FRAMES - 1) {
            assert!(!speech_frame(&mut detector));
        }
        assert!(speech_frame(&mut detector));
        assert!(detector.fired());

        // The report is consumed by the caller once; later frames must not repeat it.
        for _ in 0..REQUIRED_FRAMES {
            assert!(!speech_frame(&mut detector));
        }
    }

    #[test]
    fn impulsive_noise_never_accumulates_enough() {
        let mut detector = SpeechPresenceDetector::default();

        // A loud keyboard: short bursts that clear the level floor, repeated for a minute.
        for burst in 0..600 {
            for _ in 0..5 {
                assert!(!detector.observe(0.95, -29.0));
            }
            for _ in 0..45 {
                assert!(!detector.observe(0.3, -50.0), "burst {burst}");
            }
        }

        assert!(!detector.fired());
    }

    #[test]
    fn quiet_frames_are_ignored_even_at_full_probability() {
        let mut detector = SpeechPresenceDetector::default();

        for _ in 0..(WINDOW_FRAMES * 2) {
            assert!(!detector.observe(1.0, -60.0));
        }

        assert!(!detector.fired());
    }

    #[test]
    fn the_window_forgets_speech_that_falls_out_of_it() {
        let mut detector = SpeechPresenceDetector::default();

        for _ in 0..(REQUIRED_FRAMES - 1) {
            assert!(!speech_frame(&mut detector));
        }
        for _ in 0..WINDOW_FRAMES {
            assert!(!silent_frame(&mut detector));
        }
        for _ in 0..(REQUIRED_FRAMES - 1) {
            assert!(!speech_frame(&mut detector));
        }

        assert!(!detector.fired());
        assert!(speech_frame(&mut detector));
    }

    #[test]
    fn non_finite_measurements_do_not_qualify() {
        let mut detector = SpeechPresenceDetector::default();

        for _ in 0..(WINDOW_FRAMES * 2) {
            assert!(!detector.observe(f32::NAN, -10.0));
            assert!(!detector.observe(1.0, f32::NAN));
        }

        assert!(!detector.fired());
    }

    #[test]
    fn reset_rearms_the_detector() {
        let mut detector = SpeechPresenceDetector::default();
        for _ in 0..REQUIRED_FRAMES {
            speech_frame(&mut detector);
        }
        assert!(detector.fired());

        detector.reset();

        assert!(!detector.fired());
        for _ in 0..(REQUIRED_FRAMES - 1) {
            assert!(!speech_frame(&mut detector));
        }
        assert!(speech_frame(&mut detector));
    }
}
