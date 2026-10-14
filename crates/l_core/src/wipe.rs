//! Wipe state machine and amnesia status label (spec section 5).

/// What started the wipe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    StickRemoved,
    Hotkey,
    ControllerLost,
}

impl Trigger {
    pub fn as_str(&self) -> &'static str {
        match self {
            Trigger::StickRemoved => "unplug",
            Trigger::Hotkey => "hotkey",
            Trigger::ControllerLost => "controller",
        }
    }
}

/// Wipe stages in the order required by spec section 5.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Stage {
    Armed,
    InputLocked,
    ScreenBlanked,
    DmaStopped,
    StackSwitched,
    KeysZeroed,
    UserZeroed,
    AllocatedZeroed,
    UsableZeroed,
    KernelZeroed,
    Reported,
    PowerOff,
    ResetFallback,
    Halted,
}

pub const ORDER: [Stage; 14] = [
    Stage::Armed,
    Stage::InputLocked,
    Stage::ScreenBlanked,
    Stage::DmaStopped,
    Stage::StackSwitched,
    Stage::KeysZeroed,
    Stage::UserZeroed,
    Stage::AllocatedZeroed,
    Stage::UsableZeroed,
    Stage::KernelZeroed,
    Stage::Reported,
    Stage::PowerOff,
    Stage::ResetFallback,
    Stage::Halted,
];

impl Stage {
    pub fn next(self) -> Option<Stage> {
        ORDER.get(self as usize + 1).copied()
    }

    pub fn from_u8(v: u8) -> Option<Stage> {
        ORDER.get(v as usize).copied()
    }
}

/// A stage was reported out of order (or before a trigger).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OrderError;

#[derive(Clone, Copy, Debug)]
pub struct WipeState {
    stage: Stage,
    trigger: Option<Trigger>,
}

impl WipeState {
    pub const fn new() -> Self {
        WipeState { stage: Stage::Armed, trigger: None }
    }

    /// Records the trigger; only the first call returns true.
    pub fn trigger(&mut self, t: Trigger) -> bool {
        if self.trigger.is_some() {
            return false;
        }
        self.trigger = Some(t);
        true
    }

    /// Completes `done`, which must be the stage right after the current one.
    pub fn advance(&mut self, done: Stage) -> Result<(), OrderError> {
        if self.trigger.is_none() || self.stage.next() != Some(done) {
            return Err(OrderError);
        }
        self.stage = done;
        Ok(())
    }

    pub fn stage(&self) -> Stage {
        self.stage
    }

    pub fn trigger_kind(&self) -> Option<Trigger> {
        self.trigger
    }
}

impl Default for WipeState {
    fn default() -> Self {
        Self::new()
    }
}

/// Preconditions for the "amnesisch" status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Amnesia {
    pub stick_sealed: bool,
    pub plan_ready: bool,
    pub serial_mirror: bool,
}

impl Amnesia {
    pub fn armed(&self) -> bool {
        self.stick_sealed && self.plan_ready && !self.serial_mirror
    }

    pub fn label(&self) -> &'static str {
        if self.armed() { "amnesisch: ja" } else { "amnesisch: nein" }
    }

    /// First failing reason (serial mirror takes precedence), "" when armed.
    /// A `test-hooks` kernel reports `plan_ready = false` (its wipe only
    /// counts), so it shows "Wipe-Plan fehlt"; the struct has no separate
    /// test-hooks input.
    pub fn reason(&self) -> &'static str {
        if self.serial_mirror {
            "Serial-Spiegel an"
        } else if !self.stick_sealed {
            "kein Boot-Stick"
        } else if !self.plan_ready {
            "Wipe-Plan fehlt"
        } else {
            ""
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(s: Stage) -> usize {
        ORDER.iter().position(|x| *x == s).unwrap()
    }

    #[test]
    fn stage_order_matches_spec_section_5() {
        assert!(pos(Stage::KeysZeroed) < pos(Stage::UserZeroed));
        assert!(pos(Stage::UserZeroed) < pos(Stage::AllocatedZeroed));
        assert!(pos(Stage::AllocatedZeroed) < pos(Stage::UsableZeroed));
        assert!(pos(Stage::UsableZeroed) < pos(Stage::KernelZeroed));
        assert!(pos(Stage::KernelZeroed) < pos(Stage::PowerOff));
        assert!(pos(Stage::PowerOff) < pos(Stage::ResetFallback));
        for z in [Stage::KeysZeroed, Stage::UserZeroed, Stage::AllocatedZeroed,
                  Stage::UsableZeroed, Stage::KernelZeroed] {
            assert!(pos(Stage::InputLocked) < pos(z));
            assert!(pos(Stage::ScreenBlanked) < pos(z));
        }
    }

    #[test]
    fn stage_next_walks_order_and_ends_at_halted() {
        let mut s = Stage::Armed;
        for expected in &ORDER[1..] {
            s = s.next().unwrap();
            assert_eq!(s, *expected);
        }
        assert_eq!(s, Stage::Halted);
        assert_eq!(s.next(), None);
    }

    #[test]
    fn stage_u8_roundtrip_and_unknown_is_none() {
        for s in ORDER {
            assert_eq!(Stage::from_u8(s as u8), Some(s));
        }
        assert_eq!(Stage::from_u8(14), None);
        assert_eq!(Stage::from_u8(255), None);
    }

    #[test]
    fn first_trigger_wins() {
        let mut w = WipeState::new();
        assert_eq!(w.stage(), Stage::Armed);
        assert_eq!(w.trigger_kind(), None);
        assert!(w.trigger(Trigger::Hotkey));
        assert!(!w.trigger(Trigger::StickRemoved));
        assert_eq!(w.trigger_kind(), Some(Trigger::Hotkey));
        assert_eq!(Trigger::StickRemoved.as_str(), "unplug");
        assert_eq!(Trigger::Hotkey.as_str(), "hotkey");
        assert_eq!(Trigger::ControllerLost.as_str(), "controller");
    }

    #[test]
    fn advance_rejects_skipping_and_going_back() {
        let mut w = WipeState::new();
        w.trigger(Trigger::Hotkey);
        assert!(w.advance(Stage::InputLocked).is_ok());
        assert!(w.advance(Stage::DmaStopped).is_err());
        assert_eq!(w.stage(), Stage::InputLocked);
        assert!(w.advance(Stage::InputLocked).is_err());
        assert!(w.advance(Stage::Armed).is_err());
        assert!(w.advance(Stage::ScreenBlanked).is_ok());
        assert_eq!(w.stage(), Stage::ScreenBlanked);
    }

    #[test]
    fn advance_before_trigger_is_error() {
        let mut w = WipeState::new();
        assert!(w.advance(Stage::InputLocked).is_err());
        assert_eq!(w.stage(), Stage::Armed);
    }

    #[test]
    fn amnesia_label_ja_only_when_all_true() {
        for bits in 0..8u8 {
            let a = Amnesia {
                stick_sealed: bits & 1 != 0,
                plan_ready: bits & 2 != 0,
                serial_mirror: bits & 4 != 0,
            };
            let expect = bits == 3;
            assert_eq!(a.armed(), expect);
            assert_eq!(a.label(), if expect { "amnesisch: ja" } else { "amnesisch: nein" });
            assert_eq!(a.reason().is_empty(), expect);
        }
    }

    #[test]
    fn serial_mirror_forces_nein_and_is_reported_first() {
        let a = Amnesia { stick_sealed: false, plan_ready: false, serial_mirror: true };
        assert!(!a.armed());
        assert_eq!(a.reason(), "Serial-Spiegel an");
        let b = Amnesia { stick_sealed: false, plan_ready: false, serial_mirror: false };
        assert_eq!(b.reason(), "kein Boot-Stick");
        let c = Amnesia { stick_sealed: true, plan_ready: false, serial_mirror: false };
        assert_eq!(c.reason(), "Wipe-Plan fehlt");
    }
}
