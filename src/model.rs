//! Who is away, and how often a player may say so.
pub use sp_travel::{Id, Location, settings::Field};
use std::collections::{BTreeMap, BTreeSet};

/// The most players away at once; one more is asked to try again.
pub const MAX_AWAY: usize = 1024;
/// The most remembered cooldowns; run-out ones are dropped first.
pub const MAX_COOLDOWNS: usize = 4096;

/// The settings file's keys, bounds and defaults, in the order of [`Config`]'s fields.
pub const FIELDS: [Field; 3] = [
    Field {
        key: "cooldown_seconds",
        min: 0,
        max: 3600,
        default: 60,
    },
    Field {
        key: "check_seconds",
        min: 1,
        max: 60,
        default: 2,
    },
    // On by default, as EssentialsX's `sleep-ignores-afk-players`.
    Field {
        key: "sleep_ignores_afk",
        min: 0,
        max: 1,
        default: 1,
    },
];

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// Seconds after going away before the same player may go away again.
    pub cooldown_seconds: u64,
    /// How often the players who are away are looked at, to see who moved. Nobody away: no look at all.
    pub check_seconds: u64,
    /// Whether a player who is away is left out of the players who must sleep for the night to pass.
    pub sleep_ignores_afk: bool,
}

impl Config {
    pub fn parse(text: &str) -> Result<Self, &'static str> {
        let values = sp_travel::settings::parse(text, &FIELDS)?;
        let [cooldown_seconds, check_seconds, sleep_ignores_afk] = values.as_slice() else {
            return Err("config_error");
        };
        Ok(Self {
            cooldown_seconds: *cooldown_seconds,
            check_seconds: *check_seconds,
            sleep_ignores_afk: *sleep_ignores_afk == 1,
        })
    }
}

/// What `/afk` did.
#[derive(Debug, PartialEq, Eq)]
pub enum Toggle {
    Away,
    Back,
    /// Too soon after the last time; this many seconds are left.
    Wait(u64),
    /// Too many players are away.
    Full,
}

/// The players who are away, with the place each one went away at, when each player last went away, and the
/// players this script left out of the sleep count.
#[derive(Default)]
pub struct Afk {
    away: BTreeMap<Id, Location>,
    last: BTreeMap<Id, u64>,
    /// Only the players this script asked the server to leave out of the sleep count: always players who are
    /// away, so it is bounded with them. A wish another plugin made is not in it and is never taken back here.
    sleep_ignored: BTreeSet<Id>,
}

impl Afk {
    /// `/afk`: back when the player is away, away otherwise. Coming back is always allowed; going away again
    /// waits for the cooldown, which a rejoin does not reset.
    pub fn toggle(&mut self, id: Id, here: Location, now_ms: u64, cooldown_seconds: u64) -> Toggle {
        if self.away.remove(&id).is_some() {
            return Toggle::Back;
        }
        let cooldown = cooldown_seconds.saturating_mul(1000);
        if let Some(last) = self.last.get(&id) {
            let ready = last.saturating_add(cooldown);
            if now_ms < ready {
                return Toggle::Wait((ready - now_ms).div_ceil(1000));
            }
        }
        if self.away.len() >= MAX_AWAY {
            return Toggle::Full;
        }
        if cooldown > 0 {
            if self.last.len() >= MAX_COOLDOWNS {
                self.last
                    .retain(|_, last| last.saturating_add(cooldown) > now_ms);
            }
            if self.last.len() >= MAX_COOLDOWNS && !self.last.contains_key(&id) {
                return Toggle::Full;
            }
            self.last.insert(id, now_ms);
        }
        self.away.insert(id, here);
        Toggle::Away
    }

    /// The player is no longer away (he moved, wrote, or left). Whether he was. The caller then takes back the
    /// sleep wish with [`Afk::release`].
    pub fn back(&mut self, id: Id) -> bool {
        self.away.remove(&id).is_some()
    }

    /// The script is about to leave this player out of the sleep count: whether it should. Only a player who is
    /// away, and only once.
    pub fn ignore(&mut self, id: Id) -> bool {
        self.away.contains_key(&id) && self.sleep_ignored.insert(id)
    }

    /// The player is back or gone: whether this script had left him out of the sleep count and must count him
    /// again. False for a player it never left out, whatever another plugin asked for him.
    pub fn release(&mut self, id: Id) -> bool {
        self.sleep_ignored.remove(&id)
    }

    pub fn any(&self) -> bool {
        !self.away.is_empty()
    }

    /// Everybody who is away and where he went away, for the look that sees who moved.
    pub fn away(&self) -> Vec<(Id, Location)> {
        self.away.iter().map(|(id, at)| (*id, at.clone())).collect()
    }

    /// Unload: nobody is away and nothing is remembered. The players this script left out of the sleep count,
    /// to be counted again.
    pub fn clear(&mut self) -> Vec<Id> {
        self.away.clear();
        self.last.clear();
        std::mem::take(&mut self.sleep_ignored)
            .into_iter()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at() -> Location {
        Location {
            world: "world".into(),
            pos: (1.5, 64.0, -2.5),
            yaw: 0.0,
            pitch: 0.0,
        }
    }

    #[test]
    fn afk_toggles_and_going_away_again_waits_for_the_cooldown() {
        let mut afk = Afk::default();
        let id = (1, 2);
        assert_eq!(afk.toggle(id, at(), 1_000, 60), Toggle::Away);
        assert!(afk.any());
        assert_eq!(afk.toggle(id, at(), 2_000, 60), Toggle::Back);
        assert!(!afk.any());
        assert_eq!(afk.toggle(id, at(), 2_500, 60), Toggle::Wait(59));
        assert_eq!(afk.toggle(id, at(), 60_999, 60), Toggle::Wait(1));
        assert_eq!(afk.toggle(id, at(), 61_000, 60), Toggle::Away);
        // Another player is not held by the first one's cooldown.
        assert_eq!(afk.toggle((3, 4), at(), 61_000, 60), Toggle::Away);
        // Without a cooldown nothing is remembered.
        let mut free = Afk::default();
        assert_eq!(free.toggle(id, at(), 0, 0), Toggle::Away);
        assert_eq!(free.toggle(id, at(), 0, 0), Toggle::Back);
        assert_eq!(free.toggle(id, at(), 0, 0), Toggle::Away);
        assert!(free.last.is_empty());
    }

    #[test]
    fn moving_writing_or_leaving_ends_it_once() {
        let mut afk = Afk::default();
        let id = (1, 2);
        assert!(!afk.back(id));
        afk.toggle(id, at(), 0, 60);
        assert_eq!(afk.away(), vec![(id, at())]);
        assert!(afk.back(id));
        assert!(!afk.back(id));
        assert!(afk.away().is_empty());
        // The cooldown outlives the end of the status (a rejoin does not reset it).
        assert_eq!(afk.toggle(id, at(), 30_000, 60), Toggle::Wait(30));
    }

    #[test]
    fn only_the_sleep_wish_this_script_made_is_taken_back() {
        let mut afk = Afk::default();
        let (me, other) = ((1, 2), (3, 4));
        // A player who is not away is never left out by this script, and so never counted again by it: a wish
        // another plugin made for him stays.
        assert!(!afk.ignore(me));
        assert!(!afk.release(me));
        assert_eq!(afk.toggle(me, at(), 0, 0), Toggle::Away);
        assert!(afk.ignore(me));
        assert!(!afk.ignore(me), "asked once");
        // Coming back: counted again, once.
        assert_eq!(afk.toggle(me, at(), 0, 0), Toggle::Back);
        assert!(afk.release(me));
        assert!(!afk.release(me));
        // Away without the setting: nothing to take back when he returns.
        assert_eq!(afk.toggle(me, at(), 0, 0), Toggle::Away);
        assert!(afk.back(me));
        assert!(!afk.release(me));
        // Leaving or unloading counts again only the players this script left out.
        assert_eq!(afk.toggle(me, at(), 0, 0), Toggle::Away);
        assert_eq!(afk.toggle(other, at(), 0, 0), Toggle::Away);
        assert!(afk.ignore(me));
        assert_eq!(afk.clear(), vec![me]);
        assert!(!afk.release(me) && !afk.release(other));
    }

    #[test]
    fn the_lists_are_bounded() {
        let mut afk = Afk::default();
        for n in 0..MAX_AWAY as u64 {
            assert_eq!(afk.toggle((n, 0), at(), 0, 60), Toggle::Away);
        }
        assert_eq!(afk.toggle((u64::MAX, 0), at(), 0, 60), Toggle::Full);
        assert!(afk.back((0, 0)));
        assert_eq!(afk.toggle((u64::MAX, 0), at(), 0, 60), Toggle::Away);

        let mut many = Afk::default();
        for n in 0..MAX_COOLDOWNS as u64 {
            assert_eq!(many.toggle((n, 1), at(), 0, 60), Toggle::Away);
            assert!(many.back((n, 1)));
        }
        // Every cooldown still runs: one more player is refused instead of an old cooldown being dropped.
        assert_eq!(many.toggle((u64::MAX, 1), at(), 59_999, 60), Toggle::Full);
        // Once they ran out they are dropped and the next player goes away.
        assert_eq!(many.toggle((u64::MAX, 1), at(), 60_000, 60), Toggle::Away);
        assert_eq!(many.last.len(), 1);
        assert!(many.clear().is_empty());
        assert!(!many.any() && many.last.is_empty());
    }

    #[test]
    fn the_default_settings_read_back_and_bad_ones_are_refused() {
        let text = sp_travel::settings::default_text(&FIELDS);
        assert_eq!(
            text,
            "cooldown_seconds=60\ncheck_seconds=2\nsleep_ignores_afk=1\n"
        );
        assert_eq!(
            Config::parse(&text).unwrap(),
            Config {
                cooldown_seconds: 60,
                check_seconds: 2,
                sleep_ignores_afk: true
            }
        );
        // A settings file written before the setting existed keeps the default.
        assert!(
            Config::parse("cooldown_seconds=60\ncheck_seconds=2\n")
                .unwrap()
                .sleep_ignores_afk
        );
        assert!(
            !Config::parse("sleep_ignores_afk=0")
                .unwrap()
                .sleep_ignores_afk
        );
        for bad in [
            "check_seconds=0",
            "cooldown_seconds=3601",
            "afk=1",
            "sleep_ignores_afk=2",
        ] {
            assert_eq!(Config::parse(bad), Err("config_error"), "{bad}");
        }
    }
}
