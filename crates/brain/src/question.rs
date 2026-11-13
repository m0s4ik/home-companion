// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The Yes/No question before house commands that need a yes (covers, valves). The brain owns
//! it: the model asks once (`home_control`), then every answer, from the screen, a reflex, the
//! stop word or Home Assistant's transcript, goes through one door ([`Question::answer`]) and
//! the brain carries the command out itself. The model decides nothing after asking.
//!
//! A question is recognised by its actions ([`crate::house::key_of`]), not by an id the model
//! carries. A spoken yes waits a second before anything moves (a "no" or the stop word inside
//! that second wins); a yes on the screen is carried out at once. This module is the pure state
//! machine: time comes in, no Home Assistant, no threads.

use crate::house::Action;

/// How long a question waits for an answer (ms).
pub const ASK_MS: i64 = 30_000;
/// How long a spoken yes waits before the command runs, so a "no" can still win (ms): two
/// seconds, a person needs about 1.4 s to see it and touch No (measured on the device).
pub const CONFIRM_DELAY_MS: i64 = 2_000;

/// What the model is told when the question is on the screen.
pub const SAY_NOTHING: &str = "nothing: the device asks the question and hears the answer itself; \
     you will not see the answer and nothing is pending for you. Every later request, even the same \
     one, is a new call to home_control";

/// Where an answer came from (or why the question closed without one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    Screen,
    VoiceReflex,
    VoiceHa,
    Stop,
    Timeout,
    Replaced,
}

impl Via {
    /// The name on the wire (`confirm.answer`, `confirm_done`).
    pub fn name(self) -> &'static str {
        match self {
            Via::Screen => "screen",
            Via::VoiceReflex => "voice_reflex",
            Via::VoiceHa => "voice_ha",
            Via::Stop => "stop",
            Via::Timeout => "timeout",
            Via::Replaced => "replaced",
        }
    }

    /// A `via` the body may send. The brain's own reasons are not answers: voice_ha is built
    /// by `Brain::turn` from Home Assistant's transcript, timeout and replaced by the brain.
    pub fn parse(s: &str) -> Option<Via> {
        Some(match s {
            "screen" => Via::Screen,
            "voice_reflex" => Via::VoiceReflex,
            "stop" => Via::Stop,
            _ => return None,
        })
    }

    fn spoken(self) -> bool {
        matches!(self, Via::VoiceReflex | Via::VoiceHa)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    /// Asked, nobody answered yet.
    Open,
    /// A spoken yes: it runs at `until_ms` unless a no comes first.
    Confirming {
        until_ms: i64,
        via: Via,
    },
    /// The yes is final; the commands run (or are about to).
    Executing {
        via: Via,
    },
    /// Done; `failed` = the full names of the devices that did not do it.
    Yes {
        via: Via,
        failed: Vec<String>,
    },
    No {
        via: Via,
    },
    /// Nobody answered in time, or another question took its place.
    Expired {
        via: Via,
    },
}

impl State {
    pub fn name(&self) -> &'static str {
        match self {
            State::Open => "open",
            State::Confirming { .. } => "confirming",
            State::Executing { .. } => "executing",
            State::Yes { .. } => "yes",
            State::No { .. } => "no",
            State::Expired { .. } => "expired",
        }
    }

    /// The state as `home_control` reports it to the model when its call matches this
    /// question: "waiting", "confirming", "done", "answered_no", "expired".
    pub fn reported(&self) -> &'static str {
        match self {
            State::Open => "waiting",
            State::Confirming { .. } => "confirming",
            State::Executing { .. } | State::Yes { .. } => "done",
            State::No { .. } => "answered_no",
            State::Expired { .. } => "expired",
        }
    }

    /// What the model is told to say then, when it matters.
    pub fn say(&self) -> Option<&'static str> {
        match self {
            State::Open | State::Confirming { .. } => Some(SAY_NOTHING),
            State::No { .. } => {
                Some("the user said no: nothing was done; say so in two words at most")
            }
            State::Expired { .. } => Some("the question expired: nothing was done"),
            _ => None,
        }
    }

    /// Still open for an answer.
    pub fn answerable(&self) -> bool {
        matches!(self, State::Open | State::Confirming { .. })
    }
}

/// What an answer changed (the brain turns it into events).
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Nothing: a late, repeated or meaningless answer.
    Ignored,
    /// A spoken yes: it runs at `until_ms` (`confirm_pending`).
    Pending { until_ms: i64 },
    /// A yes on the screen: run it now.
    Execute,
    /// A no: closed (`confirm_done` with yes false).
    No,
}

#[derive(Debug, Clone)]
pub struct Question {
    pub id: i64,
    pub key: Vec<(String, String, String)>,
    pub actions: Vec<Action>,
    pub text: String,
    /// The full name of each action's device.
    pub names: Vec<String>,
    pub asked_ms: i64,
    /// The brain's sequence number of the turn that asked (None: no turn was open).
    pub turn: Option<u64>,
    pub state: State,
}

impl Question {
    pub fn new(
        id: i64,
        actions: Vec<Action>,
        text: String,
        asked_ms: i64,
        turn: Option<u64>,
    ) -> Self {
        Question {
            id,
            key: crate::house::key_of(&actions),
            names: actions.iter().map(|a| a.entity.full_name()).collect(),
            actions,
            text,
            asked_ms,
            turn,
            state: State::Open,
        }
    }

    /// When the question closes unanswered (unix s), as the screen shows it.
    pub fn expires_at(&self) -> i64 {
        (self.asked_ms + ASK_MS) / 1000
    }

    /// The one door for answers. Open: a no closes, a yes on the screen runs at once, a spoken
    /// yes waits [`CONFIRM_DELAY_MS`]. Confirming: a no still wins, a yes on the screen runs at
    /// once, another spoken yes changes nothing. Anything else: ignored.
    pub fn answer(&mut self, yes: bool, via: Via, now_ms: i64) -> Effect {
        match (&self.state, yes) {
            (State::Open | State::Confirming { .. }, false) => {
                self.state = State::No { via };
                Effect::No
            }
            (State::Open | State::Confirming { .. }, true) if via == Via::Screen => {
                self.state = State::Executing { via };
                Effect::Execute
            }
            (State::Open, true) if via.spoken() => {
                let until_ms = now_ms + CONFIRM_DELAY_MS;
                self.state = State::Confirming { until_ms, via };
                Effect::Pending { until_ms }
            }
            _ => Effect::Ignored,
        }
    }

    /// Unanswered for too long: closed.
    pub fn expired(&self, now_ms: i64) -> bool {
        self.state == State::Open && now_ms - self.asked_ms > ASK_MS
    }

    /// A spoken yes whose second has passed: it becomes final.
    pub fn confirm_due(&mut self, now_ms: i64) -> bool {
        if let State::Confirming { until_ms, via } = self.state
            && now_ms >= until_ms
        {
            self.state = State::Executing { via };
            return true;
        }
        false
    }

    /// The next moment this question needs the clock (ms).
    pub fn deadline(&self) -> Option<i64> {
        match self.state {
            State::Open => Some(self.asked_ms + ASK_MS + 1),
            State::Confirming { until_ms, .. } => Some(until_ms),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q() -> Question {
        Question::new(1, Vec::new(), "Chiudo X?".into(), 1_000, Some(3))
    }

    #[test]
    fn a_spoken_yes_waits_and_a_no_wins() {
        let mut a = q();
        assert_eq!(
            a.answer(true, Via::VoiceReflex, 2_000),
            Effect::Pending {
                until_ms: 2_000 + CONFIRM_DELAY_MS
            }
        );
        assert_eq!(a.answer(true, Via::VoiceHa, 2_100), Effect::Ignored);
        assert_eq!(a.deadline(), Some(2_000 + CONFIRM_DELAY_MS));
        assert!(!a.confirm_due(2_000 + CONFIRM_DELAY_MS - 1));
        assert_eq!(a.answer(false, Via::Stop, 2_500), Effect::No);
        assert_eq!(a.state, State::No { via: Via::Stop });
        assert!(!a.confirm_due(5_000));
        let mut b = q();
        b.answer(true, Via::VoiceHa, 2_000);
        assert!(b.confirm_due(2_000 + CONFIRM_DELAY_MS));
        assert_eq!(b.state, State::Executing { via: Via::VoiceHa });
        assert_eq!(
            b.answer(false, Via::Screen, 2_001 + CONFIRM_DELAY_MS),
            Effect::Ignored
        );
    }

    #[test]
    fn a_screen_yes_runs_at_once_also_inside_the_second() {
        let mut a = q();
        assert_eq!(a.answer(true, Via::Screen, 2_000), Effect::Execute);
        assert_eq!(a.answer(true, Via::Screen, 2_001), Effect::Ignored);
        let mut b = q();
        b.answer(true, Via::VoiceReflex, 2_000);
        assert_eq!(b.answer(true, Via::Screen, 2_500), Effect::Execute);
        assert_eq!(b.state, State::Executing { via: Via::Screen });
    }

    #[test]
    fn unanswered_questions_expire_after_thirty_seconds() {
        let a = q();
        assert_eq!(a.expires_at(), 31);
        assert!(!a.expired(31_000));
        assert!(a.expired(31_001));
        assert_eq!(a.deadline(), Some(31_001));
    }

    #[test]
    fn only_the_body_answers_names() {
        for v in [Via::Screen, Via::VoiceReflex, Via::Stop] {
            assert_eq!(Via::parse(v.name()), Some(v));
        }
        assert_eq!(Via::parse("voice_ha"), None);
        assert_eq!(Via::parse("timeout"), None);
        assert_eq!(Via::parse("replaced"), None);
        // A yes "by the stop word" means nothing.
        assert_eq!(q().answer(true, Via::Stop, 2_000), Effect::Ignored);
    }
}
