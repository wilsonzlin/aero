#[derive(Debug, Clone)]
pub struct VgaTiming {
    frame_ns: u64,
    vblank_ns: u64,
    frame_time_ns: u64,

    in_vblank: bool,
    display_enabled: bool,

    text_blink_toggle_ns: u64,
    text_blink_accum_ns: u64,
    text_blink_state_on: bool,

    cursor_blink_toggle_ns: u64,
    cursor_blink_accum_ns: u64,
    cursor_blink_state_on: bool,
}

impl Default for VgaTiming {
    fn default() -> Self {
        let frame_ns = 16_666_667; // 60 Hz, rounded to integer nanoseconds.
        let vblank_ns = 1_500_000; // ~1.5ms vblank out of a 16.67ms frame.
        let frame_time_ns = 0;

        let in_vblank = false;
        let display_enabled = true;

        let text_blink_toggle_ns = 500_000_000; // 1Hz blink (toggle every 0.5s).
        let text_blink_accum_ns = 0;
        let text_blink_state_on = true;

        let cursor_blink_toggle_ns = 500_000_000;
        let cursor_blink_accum_ns = 0;
        let cursor_blink_state_on = true;

        Self {
            frame_ns,
            vblank_ns,
            frame_time_ns,
            in_vblank,
            display_enabled,
            text_blink_toggle_ns,
            text_blink_accum_ns,
            text_blink_state_on,
            cursor_blink_toggle_ns,
            cursor_blink_accum_ns,
            cursor_blink_state_on,
        }
    }
}

impl VgaTiming {
    pub fn frame_ns(&self) -> u64 {
        self.frame_ns
    }

    pub fn vblank_ns(&self) -> u64 {
        self.vblank_ns
    }

    pub fn in_vblank(&self) -> bool {
        self.in_vblank
    }

    pub fn display_enabled(&self) -> bool {
        self.display_enabled
    }

    pub fn text_blink_state_on(&self) -> bool {
        self.text_blink_state_on
    }

    pub fn cursor_blink_state_on(&self) -> bool {
        self.cursor_blink_state_on
    }

    pub fn tick(&mut self, delta_ns: u64) {
        if self.frame_ns > 0 {
            self.frame_time_ns = (self.frame_time_ns + delta_ns) % self.frame_ns;
            let vblank_start_ns = self.frame_ns.saturating_sub(self.vblank_ns);
            self.in_vblank = self.frame_time_ns >= vblank_start_ns;
            self.display_enabled = !self.in_vblank;
        }

        Self::tick_blink(
            delta_ns,
            self.text_blink_toggle_ns,
            &mut self.text_blink_accum_ns,
            &mut self.text_blink_state_on,
        );
        Self::tick_blink(
            delta_ns,
            self.cursor_blink_toggle_ns,
            &mut self.cursor_blink_accum_ns,
            &mut self.cursor_blink_state_on,
        );
    }

    fn tick_blink(delta_ns: u64, toggle_ns: u64, accum_ns: &mut u64, state: &mut bool) {
        if toggle_ns == 0 {
            return;
        }

        let total = *accum_ns + delta_ns;
        let toggles = total / toggle_ns;
        *accum_ns = total % toggle_ns;

        if (toggles & 1) == 1 {
            *state = !*state;
        }
    }
}
