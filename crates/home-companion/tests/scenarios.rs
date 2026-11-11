// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The body's side of the Yes/No question and of the reflex gate, row by row (C01-C25): each
//! test a sequence seen for real (the model's ways of asking, the answering channels, the
//! timings), the core in-process against the fake Home Assistant, the brain's events injected
//! as the screen forwards them, and the brain's port recorded. C24 is in src/ui/house.rs.
//! The question's id is 42; the conversations are numbered from 1.

mod common;

use common::*;
use home_companion::core::{Phase, UiEvent};
use serde_json::{Value, json};
use wyoming::fake_ha::{Scenario, Step, answer_wav};

fn sc(steps: Vec<Step>) -> Scenario {
    Scenario { ping: false, steps }
}

fn hello() -> Vec<Step> {
    vec![Step::Connect, Step::RunSatellite, Step::Sleep { ms: 300 }]
}

/// The satellite's next run: its audio (at least `min_ms`) and Home Assistant's transcript.
fn hear(text: &str, min_ms: u64) -> Vec<Step> {
    vec![
        Step::WaitDetection { timeout_ms: 9000 },
        Step::CollectAudio {
            min_ms,
            max_ms: 12000,
        },
        Step::SendTranscript { text: text.into() },
    ]
}

/// Home Assistant's answer: its words, its voice, and the satellite's `played`.
fn answer(text: &str) -> Vec<Step> {
    vec![
        Step::SendSynthesize { text: text.into() },
        Step::SendWav { path: answer_wav() },
        Step::ExpectPlayed { timeout_ms: 4000 },
    ]
}

fn bye(ms: u64) -> Vec<Step> {
    vec![Step::Sleep { ms }, Step::Disconnect]
}

fn cat(parts: Vec<Vec<Step>>) -> Vec<Step> {
    parts.into_iter().flatten().collect()
}

fn question() -> UiEvent {
    UiEvent::Question(42)
}

/// The asking request: "chiudi la tapparella" (not a reflex: to Home Assistant), its
/// transcript, then (the model calls the tool) the brain's question 42 on the screen while the
/// conversation thinks; `late` = what Home Assistant still sends for that run `pause_ms` later.
fn asking(pause_ms: u64, late: Vec<Step>, rest: Vec<Step>) -> Scenario {
    sc(cat(vec![
        hello(),
        hear("chiudi la tapparella", 800),
        vec![Step::Sleep { ms: pause_ms }],
        late,
        rest,
    ]))
}

/// The asking run's late answer (the spoken question), swallowed.
fn late_question() -> Vec<Step> {
    answer("Chiudo la tapparella del soggiorno?")
}

/// Talk, then the question when the brain has the asking turn.
fn asking_presses(more: &[Press]) -> Vec<Press> {
    let mut p = vec![TALK_AT_HOME, press(When::Port("turn", 1), 50, question)];
    p.extend_from_slice(more);
    p
}

fn chiudi() -> Utt {
    Utt::says("chiudi la tapparella")
}

fn confirm(yes: bool, via: &str) -> Value {
    json!({"cmd": "confirm.answer", "id": 42, "yes": yes, "via": via})
}

fn pair(a: &str, b: &str) -> (String, String) {
    (a.to_string(), b.to_string())
}

fn assert_clean(r: &Run) {
    assert!(r.record.failures.is_empty(), "{:?}", r.record.failures);
    assert!(
        !r.views.contains(&Phase::Error),
        "no error view: {:?}",
        r.views
    );
}

/// The answering window after the asking request, its utterance `utt`, the asking run's late
/// answer 1.5 s after the transcript.
fn window(utt: Utt, more: &[Press]) -> Run {
    run_setup(
        Setup::new(
            asking(1500, late_question(), bye(2000)),
            asking_presses(more),
        )
        .utts(vec![chiudi(), utt]),
    )
}

#[test]
fn c01_the_question_opens_the_microphone_and_swallows_the_asking_answer() {
    let r = window(Utt::says("sì"), &[]);
    assert_clean(&r);
    assert_eq!(
        r.outcomes(),
        vec![pair("talk", "question"), pair("follow_up", "reflex")]
    );
    // The spoken question was never shown nor played; its audio-stop got `played`, and no
    // second run-pipeline came before it (the fake checks it).
    assert!(r.answers().is_empty(), "{:?}", r.answers());
    assert_eq!(r.record.run_pipelines.len(), 1);
    assert_eq!(r.received("played"), 1);
    // The asking turn ended with its run (nothing said: it was not heard; see C25).
    assert_eq!(
        r.port("turn"),
        vec![json!({"cmd": "turn", "id": 1, "text": "chiudi la tapparella", "answers": null})]
    );
    assert_eq!(
        r.port("turn.end"),
        vec![json!({"cmd": "turn.end", "id": 1, "said": ""})]
    );
}

#[test]
fn c02_a_spoken_yes_in_the_window_answers_by_reflex() {
    let r = window(Utt::says("sì"), &[]);
    assert_clean(&r);
    assert_eq!(
        r.port("confirm.answer"),
        vec![confirm(true, "voice_reflex")]
    );
    assert_eq!(r.summary.detections_sent, 1, "no run-pipeline for the yes");
    assert_eq!(r.outcomes()[1], pair("follow_up", "reflex"));
}

#[test]
fn c03_a_spoken_no_or_basta_in_the_window_answers_no() {
    for word in ["no", "basta"] {
        let r = window(Utt::says(word), &[]);
        assert_clean(&r);
        assert_eq!(
            r.port("confirm.answer"),
            vec![confirm(false, "voice_reflex")],
            "{word}"
        );
        assert_eq!(r.summary.detections_sent, 1, "{word}");
        assert_eq!(r.outcomes()[1], pair("follow_up", "reflex"), "{word}");
    }
}

/// The question on the screen, a new conversation (Talk): it answers nothing.
fn new_conversation(utt: Utt, rest: Vec<Step>) -> Run {
    run_setup(
        Setup::new(
            sc(cat(vec![hello(), rest])),
            vec![
                press(When::View(Phase::Home), 100, question),
                press(When::Ms(300), 0, talk),
            ],
        )
        .utts(vec![utt]),
    )
}

#[test]
fn c04_yes_in_a_new_conversation_goes_to_home_assistant() {
    let r = new_conversation(
        Utt::says("sì"),
        cat(vec![hear("sì", 800), answer("Va bene."), bye(500)]),
    );
    assert_clean(&r);
    assert!(r.port("confirm.answer").is_empty());
    assert_eq!(
        r.port("turn"),
        vec![json!({"cmd": "turn", "id": 1, "text": "sì", "answers": null})]
    );
    assert_eq!(r.summary.detections_sent, 1);
    assert_eq!(r.outcomes(), vec![pair("talk", "played")]);
}

#[test]
fn c04_no_in_a_new_conversation_ends_quietly() {
    let r = new_conversation(Utt::says("no"), bye(3500));
    assert_clean(&r);
    assert!(r.port.is_empty(), "{:?}", r.port);
    assert_eq!(r.summary.detections_sent, 0);
    assert_eq!(r.outcomes(), vec![pair("talk", "reflex")]);
}

#[test]
fn c05_stop_in_the_window_is_a_no() {
    let r = window(
        Utt::silent(),
        &[press(When::View(Phase::Listening), 300, stop)],
    );
    assert_clean(&r);
    assert_eq!(r.port("confirm.answer"), vec![confirm(false, "stop")]);
    assert_eq!(r.outcomes()[1], pair("follow_up", "stopped"));
}

#[test]
fn c05_stop_in_another_conversation_answers_nothing() {
    let r = run_setup(
        Setup::new(
            sc(cat(vec![hello(), bye(2500)])),
            vec![
                press(When::View(Phase::Home), 100, question),
                press(When::Ms(200), 0, talk),
                press(When::View(Phase::Listening), 300, stop),
            ],
        )
        .utts(vec![Utt::silent()]),
    );
    assert_clean(&r);
    assert!(r.port("confirm.answer").is_empty());
    assert_eq!(r.outcomes(), vec![pair("talk", "stopped")]);
}

#[test]
fn c05_stop_over_a_ringing_timer_stops_the_bell_only() {
    let r = run_setup(
        Setup::new(
            sc(cat(vec![hello(), bye(3500)])),
            vec![
                press(When::View(Phase::Home), 100, || {
                    UiEvent::TimerStart(1, None)
                }),
                press(When::View(Phase::Ringing), 100, question),
                press(When::Ms(200), 0, stop),
            ],
        )
        .utts(vec![]),
    );
    assert_clean(&r);
    assert_eq!(r.summary.rung.len(), 1);
    assert!(r.port("confirm.answer").is_empty());
    let ringing = r.views.iter().rposition(|p| *p == Phase::Ringing).unwrap();
    assert!(
        r.views[ringing..].contains(&Phase::Home),
        "the bell stopped: {:?}",
        r.views
    );
    assert!(r.summary.conversations.is_empty());
}

#[test]
fn c06_a_tap_in_the_window_answers_once_and_ends_it() {
    for yes in [true, false] {
        let tap: fn() -> UiEvent = if yes {
            || UiEvent::QuestionAnswer { id: 42, yes: true }
        } else {
            || UiEvent::QuestionAnswer { id: 42, yes: false }
        };
        let r = window(
            Utt::silent(),
            &[press(When::View(Phase::Listening), 300, tap)],
        );
        assert_clean(&r);
        assert_eq!(r.port("confirm.answer"), vec![confirm(yes, "screen")]);
        assert_eq!(r.outcomes()[1], pair("follow_up", "question"));
        assert_eq!(r.summary.detections_sent, 1);
    }
}

#[test]
fn c07_the_confirming_second_rings_once_and_stop_takes_the_yes_back() {
    let r = run_setup(
        Setup::new(
            sc(cat(vec![hello(), bye(3500)])),
            vec![
                press(When::View(Phase::Home), 100, || {
                    UiEvent::QuestionPending(42, now_ms() + 1000)
                }),
                press(When::Ms(50), 0, || UiEvent::HouseDone),
                // At home, inside the second: the stop word's No.
                press(When::Ms(100), 0, stop),
                press(When::Ms(1300), 0, || UiEvent::QuestionClosed {
                    id: 42,
                    yes: Some(false),
                    via: "stop".into(),
                }),
                // After the second and the close: nothing to answer.
                press(When::Ms(100), 0, stop),
            ],
        )
        .utts(vec![]),
    );
    assert_clean(&r);
    assert_eq!(r.cues("success"), 1, "{:?}", r.summary.cues);
    assert_eq!(r.port("confirm.answer"), vec![confirm(false, "stop")]);
}

#[test]
fn c08_the_window_closes_unanswered_quietly() {
    let r = run_setup(
        Setup::new(
            asking(
                1500,
                late_question(),
                cat(vec![hear("sì", 800), answer("Va bene."), bye(500)]),
            ),
            asking_presses(&[
                press(When::View(Phase::Listening), 300, || {
                    UiEvent::QuestionClosed {
                        id: 42,
                        yes: None,
                        via: "timeout".into(),
                    }
                }),
                // Later, a "sì" in a new conversation is for Home Assistant (C04).
                press(When::View(Phase::Home), 300, talk),
            ]),
        )
        .utts(vec![chiudi(), Utt::silent(), Utt::says("sì")]),
    );
    assert_clean(&r);
    assert_eq!(
        r.outcomes(),
        vec![
            pair("talk", "question"),
            pair("follow_up", "question"),
            pair("talk", "played")
        ]
    );
    assert!(r.port("confirm.answer").is_empty());
    assert_eq!(r.cues("error"), 0);
    assert_eq!(r.cues("not_understood"), 0);
    // The window's end sent nothing to Home Assistant; the "sì" waited for the asking run.
    assert_eq!(r.summary.detections_sent, 2);
}

/// The window released to Home Assistant (the reflexes were unsure of the "sì"): its transcript
/// `heard`, then what the brain did (`events`), then Home Assistant's `said`.
fn window_with_ha(heard: &str, said: &str, events: &[Press]) -> Run {
    let mut presses = vec![];
    presses.extend_from_slice(events);
    run_setup(
        Setup::new(
            asking(
                1500,
                late_question(),
                cat(vec![
                    hear(heard, 800),
                    vec![Step::Sleep { ms: 800 }],
                    answer(said),
                    bye(2500),
                ]),
            ),
            asking_presses(&presses),
        )
        .utts(vec![chiudi(), Utt::says("sì").conf(0.5)]),
    )
}

#[test]
fn c09_a_yes_heard_by_home_assistant_swallows_its_done() {
    let r = window_with_ha(
        "sì",
        "Fatto.",
        &[
            press(When::Port("turn", 2), 50, || {
                UiEvent::QuestionPending(42, now_ms() + 1000)
            }),
            press(When::Ms(100), 0, || UiEvent::HouseDone),
            press(When::Ms(100), 0, || UiEvent::QuestionClosed {
                id: 42,
                yes: Some(true),
                via: "voice_ha".into(),
            }),
        ],
    );
    assert_clean(&r);
    assert_eq!(
        r.port("turn")[1],
        json!({"cmd": "turn", "id": 2, "text": "sì", "answers": 42})
    );
    let ends: Vec<Value> = r
        .port("turn.end")
        .into_iter()
        .filter(|t| t["id"] == 2)
        .collect();
    assert_eq!(ends.len(), 1, "{ends:?}");
    assert!(!r.answers().contains(&"Fatto.".to_string()));
    // Played (acknowledged), no follow-up conversation after it.
    assert_eq!(r.received("played"), 2);
    assert_eq!(
        r.outcomes(),
        vec![pair("talk", "question"), pair("follow_up", "question")]
    );
    // The yes's own sound once; the done card that follows adds none.
    assert_eq!(r.cues("success"), 1, "{:?}", r.summary.cues);
    assert!(r.port("confirm.answer").is_empty(), "the brain answered it");
}

#[test]
fn c10_something_else_in_the_window_plays_normally() {
    let r = window_with_ha("che ore sono", "Sono le dieci.", &[]);
    assert_clean(&r);
    assert_eq!(
        r.port("turn")[1],
        json!({"cmd": "turn", "id": 2, "text": "che ore sono", "answers": 42})
    );
    assert!(r.answers().contains(&"Sono le dieci.".to_string()));
    assert_eq!(r.outcomes()[1], pair("follow_up", "played"));
}

#[test]
fn c11_the_models_words_after_a_no_are_swallowed() {
    let r = window_with_ha(
        "no",
        "Va bene.",
        &[press(When::Port("turn", 2), 50, || {
            UiEvent::QuestionClosed {
                id: 42,
                yes: Some(false),
                via: "voice_ha".into(),
            }
        })],
    );
    assert_clean(&r);
    assert!(!r.answers().contains(&"Va bene.".to_string()));
    assert_eq!(
        r.outcomes(),
        vec![pair("talk", "question"), pair("follow_up", "question")]
    );
}

#[test]
fn c12_the_asking_runs_error_is_swallowed() {
    let r = run_setup(
        Setup::new(
            asking(
                200,
                vec![Step::SendError {
                    text: "intent failed".into(),
                    code: "intent-failed".into(),
                }],
                bye(2500),
            ),
            asking_presses(&[]),
        )
        .utts(vec![chiudi(), Utt::says("sì")]),
    );
    assert_clean(&r);
    assert_eq!(r.cues("error"), 0);
    assert_eq!(
        r.outcomes(),
        vec![pair("talk", "question"), pair("follow_up", "reflex")]
    );
    assert_eq!(
        r.port("confirm.answer"),
        vec![confirm(true, "voice_reflex")]
    );
}

#[test]
fn c13_a_run_that_never_ends_is_given_up_after_three_seconds() {
    // No reflexes: the gate only waits for the previous run.
    let r = run_setup(
        Setup::new(
            sc(cat(vec![
                hello(),
                hear("che tempo fa", 1000),
                vec![Step::Sleep { ms: 300 }, Step::ForgetRun],
                hear("che ore sono", 1500),
                answer("Sono le dieci."),
                bye(500),
            ])),
            vec![
                TALK_AT_HOME,
                press(When::Port("turn", 1), 50, stop),
                press(When::View(Phase::Home), 50, talk),
            ],
        )
        .mic(vec![Utt::says(""), Utt::says("").speech(&[(0.3, 3.0)])]),
    );
    assert_clean(&r);
    let transcript = r
        .record
        .sent
        .iter()
        .find(|s| s.1 == "transcript")
        .unwrap()
        .0;
    assert_eq!(r.record.run_pipelines.len(), 2);
    let gap = r.record.run_pipelines[1] - transcript;
    assert!((2.9..5.0).contains(&gap), "{gap} s");
    assert_eq!(
        r.outcomes(),
        vec![pair("talk", "stopped"), pair("talk", "played")]
    );
    // The speech of the wait went with it (the buffer, then live).
    assert!(r.record.audio_seconds >= 2.5, "{}", r.record.audio_seconds);
}

#[test]
fn c14_a_plain_conversation_tells_the_brain_its_turn() {
    let r = run_setup(
        Setup::new(
            sc(cat(vec![
                hello(),
                hear("che tempo fa", 800),
                answer("Sole."),
                bye(500),
            ])),
            vec![TALK_AT_HOME],
        )
        .mic(vec![Utt::says("")]),
    );
    assert_clean(&r);
    assert_eq!(
        r.port,
        vec![
            json!({"cmd": "turn", "id": 1, "text": "che tempo fa", "answers": null}),
            json!({"cmd": "turn.end", "id": 1, "said": "Sole."}),
        ]
    );
}

#[test]
fn c14_a_stopped_conversation_ends_its_turn_once_with_nothing_said() {
    let r = run_setup(
        Setup::new(
            sc(cat(vec![
                hello(),
                hear("che tempo fa", 800),
                vec![Step::Sleep { ms: 500 }],
                answer("Sole."),
                bye(500),
            ])),
            vec![TALK_AT_HOME, press(When::Port("turn", 1), 50, stop)],
        )
        .mic(vec![Utt::says("")]),
    );
    assert_clean(&r);
    assert_eq!(
        r.port,
        vec![
            json!({"cmd": "turn", "id": 1, "text": "che tempo fa", "answers": null}),
            json!({"cmd": "turn.end", "id": 1, "said": ""}),
        ]
    );
    assert_eq!(r.outcomes(), vec![pair("talk", "stopped")]);
}

#[test]
fn c15_talk_right_after_stop_never_plays_the_old_answer() {
    let r = run_setup(
        Setup::new(
            sc(cat(vec![
                hello(),
                hear("che tempo fa", 800),
                vec![Step::Sleep { ms: 500 }],
                answer("Vecchia risposta."),
                hear("che ore sono", 800),
                answer("Sono le dieci."),
                bye(500),
            ])),
            vec![
                TALK_AT_HOME,
                press(When::Port("turn", 1), 50, stop),
                press(When::View(Phase::Home), 50, talk),
            ],
        )
        .mic(vec![Utt::says(""), Utt::says("")]),
    );
    assert_clean(&r);
    assert_eq!(r.answers(), vec!["Sono le dieci.".to_string()]);
    assert_eq!(
        r.outcomes(),
        vec![pair("talk", "stopped"), pair("talk", "played")]
    );
    assert_eq!(r.record.run_pipelines.len(), 2);
}

/// A house command done by the brain (its card: HouseDone) when the brain has the turn, then
/// Home Assistant's `after`.
fn house(after: Vec<Step>, think_s: Option<u64>) -> Run {
    let mut s = Setup::new(
        sc(cat(vec![
            hello(),
            hear("accendi la luce", 800),
            vec![Step::Sleep { ms: 400 }],
            after,
        ])),
        vec![
            TALK_AT_HOME,
            press(When::Port("turn", 1), 50, || UiEvent::HouseDone),
        ],
    )
    .mic(vec![Utt::says("")]);
    s.think_timeout = think_s.map(std::time::Duration::from_secs);
    run_setup(s)
}

#[test]
fn c16_a_short_answer_after_a_house_command_is_not_spoken() {
    let r = house(cat(vec![answer("Fatto."), bye(500)]), None);
    assert_clean(&r);
    assert_eq!(r.outcomes(), vec![pair("talk", "house_quiet")]);
    assert!(r.answers().is_empty(), "{:?}", r.answers());
    assert_eq!(r.cues("success"), 1);
}

#[test]
fn c16_silence_after_a_house_command_ends_quietly() {
    let r = house(bye(3000), Some(2));
    assert_clean(&r);
    assert_eq!(r.outcomes(), vec![pair("talk", "house_done_silent")]);
}

#[test]
fn c16_a_long_answer_after_a_house_command_is_spoken() {
    let long = "Ho acceso la luce, ma in quella stanza non trovo nessuna lampada da accendere.";
    let r = house(cat(vec![answer(long), bye(500)]), None);
    assert_clean(&r);
    assert_eq!(r.outcomes(), vec![pair("talk", "played")]);
    assert_eq!(r.answers(), vec![long.to_string()]);
}

/// The milliseconds of audio in the release's burst: the chunks that came within 30 ms of the
/// first.
fn burst_ms(r: &Run) -> usize {
    let chunks: Vec<f64> = r
        .record
        .received
        .iter()
        .filter(|m| m.1 == "audio-chunk")
        .map(|m| m.0)
        .collect();
    chunks.iter().filter(|t| **t - chunks[0] < 0.03).count() * 20
}

#[test]
fn c17_a_partial_that_is_no_reflex_releases_early_and_live() {
    let r = run_setup(
        Setup::new(
            sc(cat(vec![
                hello(),
                hear("raccontami una barzelletta divertente", 3000),
                answer("Eccone una."),
                bye(500),
            ])),
            vec![TALK_AT_HOME],
        )
        .utts(vec![
            Utt::says("raccontami una barzelletta divertente")
                .speech(&[(0.3, 4.0)])
                .partials(&[(0.8, "raccontami"), (1.0, "raccontami barzelletta")]),
        ]),
    );
    assert_clean(&r);
    // Released before the detector's end of speech (4.7 s): the buffer in one burst, then live,
    // and Home Assistant's own end of speech ended it (no audio-stop from the body).
    let burst = burst_ms(&r);
    assert!((800..2500).contains(&burst), "burst {burst} ms");
    assert_eq!(r.received("audio-stop"), 0);
    assert!(r.record.audio_seconds >= 3.0);
    assert_eq!(r.outcomes(), vec![pair("talk", "played")]);
}

#[test]
fn c18_a_prefix_of_a_phrase_is_held_for_the_reflex() {
    let r = run_setup(
        // The answer stays on the screen 5 s.
        Setup::new(sc(cat(vec![hello(), bye(7000)])), vec![TALK_AT_HOME]).utts(vec![
            Utt::says("che ore sono").partials(&[(0.8, "che"), (1.0, "che ore")]),
        ]),
    );
    assert_clean(&r);
    assert_eq!(r.summary.detections_sent, 0);
    assert!(
        r.answers().iter().any(|a| a.starts_with("Sono le")),
        "{:?}",
        r.answers()
    );
    assert_eq!(r.outcomes(), vec![pair("talk", "reflex")]);
}

#[test]
fn c19_a_long_sentence_that_starts_as_a_reflex_goes_live() {
    let r = run_setup(
        Setup::new(
            sc(cat(vec![
                hello(),
                hear("che ore sono a new york", 3000),
                answer("Le nove a New York."),
                bye(500),
            ])),
            vec![TALK_AT_HOME],
        )
        .utts(vec![
            Utt::says("che ore sono")
                .speech(&[(0.3, 4.0)])
                .partials(&[(1.0, "che ore sono"), (1.4, "che ore sono [unk]")]),
        ]),
    );
    assert_clean(&r);
    assert_eq!(r.summary.detections_sent, 1);
    assert!(!r.answers().iter().any(|a| a.starts_with("Sono le")));
    assert_eq!(r.received("audio-stop"), 0);
    assert_eq!(r.outcomes(), vec![pair("talk", "played")]);
}

#[test]
fn c20_a_pause_on_an_open_word_goes_live() {
    let r = run_setup(
        Setup::new(
            sc(cat(vec![
                hello(),
                hear("metti una sveglia alle sette", 3000),
                answer("Sveglia alle sette."),
                bye(500),
            ])),
            vec![TALK_AT_HOME],
        )
        .utts(vec![
            Utt::says("metti una sveglia alle").speech(&[(0.3, 1.3), (2.6, 3.2)]),
        ]),
    );
    assert_clean(&r);
    assert_eq!(r.summary.detections_sent, 1);
    assert_eq!(r.received("audio-stop"), 0, "live: Home Assistant ends it");
    assert!(r.record.audio_seconds >= 3.0);
}

#[test]
fn c21_only_noise_ends_quietly() {
    let r = run_setup(
        Setup::new(sc(cat(vec![hello(), bye(4500)])), vec![TALK_AT_HOME]).utts(vec![Utt::silent()]),
    );
    assert_clean(&r);
    assert_eq!(r.summary.detections_sent, 0);
    assert_eq!(r.outcomes(), vec![pair("talk", "no speech")]);
    assert_eq!(r.cues("not_understood"), 1);
}

#[test]
fn c21_the_helper_lost_in_the_middle_releases() {
    let r = run_setup(
        Setup::new(
            sc(cat(vec![
                hello(),
                hear("che ore sono", 1500),
                answer("Sono le dieci."),
                bye(500),
            ])),
            vec![TALK_AT_HOME],
        )
        .utts(vec![
            Utt::says("che ore sono").speech(&[(0.3, 3.0)]).lost_at(1.0),
        ]),
    );
    assert_clean(&r);
    assert_eq!(r.summary.detections_sent, 1);
    assert_eq!(r.outcomes(), vec![pair("talk", "played")]);
}

/// A plain conversation whose answer is `said`, then `rest`.
fn answered(said: &str, rest: Vec<Step>) -> Run {
    run_setup(
        Setup::new(
            sc(cat(vec![
                hello(),
                hear("ricordami di chiamare", 800),
                answer(said),
                rest,
            ])),
            vec![TALK_AT_HOME],
        )
        .mic(vec![Utt::says(""), Utt::says("")]),
    )
}

#[test]
fn c22_a_real_question_listens_again() {
    let r = answered(
        "A che ora?",
        cat(vec![hear("alle cinque", 800), answer("Va bene."), bye(500)]),
    );
    assert_clean(&r);
    assert_eq!(
        r.outcomes(),
        vec![pair("talk", "played"), pair("follow_up", "played")]
    );
}

#[test]
fn c22_a_courtesy_question_does_not() {
    let r = answered("Fatto. C'è altro?", bye(2000));
    assert_clean(&r);
    assert_eq!(r.outcomes(), vec![pair("talk", "played")]);
}

#[test]
fn c22_an_empty_answer_does_not() {
    let r = answered("", bye(2000));
    assert_clean(&r);
    assert_eq!(r.outcomes(), vec![pair("talk", "played")]);
}

/// Do not disturb does not silence the success sound of a spoken yes received, as it does not
/// silence a house command's (the rule: the user just asked and waits for it; only the
/// speaker busy with an answer or the bell holds it back).
#[test]
fn c23_do_not_disturb_keeps_the_yes_received_sound() {
    let r = run_setup(
        Setup::new(
            sc(cat(vec![hello(), bye(2500)])),
            vec![
                press(When::View(Phase::Home), 100, || {
                    UiEvent::DoNotDisturb(Some(now_ms() / 1000 + 3600))
                }),
                press(When::Ms(100), 0, question),
                press(When::Ms(100), 0, || {
                    UiEvent::QuestionPending(42, now_ms() + 1000)
                }),
            ],
        )
        .utts(vec![]),
    );
    assert_clean(&r);
    assert_eq!(r.cues("success"), 1, "{:?}", r.summary.cues);
    // At home the question only shows: no conversation.
    assert!(r.summary.conversations.is_empty());
}

/// The asking turn's `turn.end`s and their times, and the first time the fake sent `kind`
/// after the transcript of the asking request.
fn asking_end(r: &Run, kind: &str) -> (Vec<Value>, f64, f64) {
    let ends: Vec<Value> = r
        .port("turn.end")
        .into_iter()
        .filter(|t| t["id"] == 1)
        .collect();
    let at: Vec<f64> = r
        .port_at
        .iter()
        .filter(|p| p.1["cmd"] == "turn.end" && p.1["id"] == 1)
        .map(|p| p.0)
        .collect();
    assert_eq!(at.len(), 1, "one turn.end of the asking turn: {ends:?}");
    let transcript = r.sent_times("transcript")[0];
    let sent = r
        .sent_times(kind)
        .into_iter()
        .find(|t| *t > transcript)
        .unwrap_or_else(|| panic!("no {kind} after the transcript"));
    (ends, at[0], sent)
}

#[test]
fn c25_the_asking_turn_ends_with_its_runs_answer() {
    // The question closes the asking conversation while Home Assistant's run still works on
    // it (the model may call the tool again): its turn stays open for the brain until the
    // run's words come, then ends once with nothing said.
    let r = window(Utt::says("sì"), &[]);
    assert_clean(&r);
    let (ends, end_at, synth_at) = asking_end(&r, "synthesize");
    assert_eq!(ends, vec![json!({"cmd": "turn.end", "id": 1, "said": ""})]);
    assert!(
        end_at >= synth_at,
        "turn.end at {end_at:.2} s, the run's answer at {synth_at:.2} s"
    );
}

#[test]
fn c25_the_asking_turn_ends_with_its_runs_error() {
    let r = run_setup(
        Setup::new(
            asking(
                1500,
                vec![Step::SendError {
                    text: "intent failed".into(),
                    code: "intent-failed".into(),
                }],
                bye(2000),
            ),
            asking_presses(&[]),
        )
        .utts(vec![chiudi(), Utt::says("sì")]),
    );
    assert_clean(&r);
    let (ends, end_at, error_at) = asking_end(&r, "error");
    assert_eq!(ends, vec![json!({"cmd": "turn.end", "id": 1, "said": ""})]);
    assert!(
        end_at >= error_at,
        "turn.end at {end_at:.2} s, the run's error at {error_at:.2} s"
    );
}

#[test]
fn c25_the_asking_turn_ends_at_the_cap_of_a_run_that_never_ends() {
    // The run never ends; the answer is no reflex (unsure): it waits for the run, at most 3 s,
    // and only then the asking turn ends and the answer's run opens.
    let r = run_setup(
        Setup::new(
            asking(
                0,
                vec![Step::ForgetRun],
                cat(vec![hear("forse", 800), answer("Come vuole."), bye(2500)]),
            ),
            asking_presses(&[]),
        )
        .utts(vec![chiudi(), Utt::says("forse").conf(0.5)]),
    );
    assert_clean(&r);
    let (ends, end_at, _) = asking_end(&r, "transcript");
    assert_eq!(ends, vec![json!({"cmd": "turn.end", "id": 1, "said": ""})]);
    let transcript = r.sent_times("transcript")[0];
    assert!(
        end_at - transcript >= 2.5,
        "turn.end {:.2} s after the asking transcript",
        end_at - transcript
    );
    assert_eq!(r.record.run_pipelines.len(), 2);
    let second = &r.port("turn")[1];
    assert_eq!(second["answers"], 42, "{second}");
}
