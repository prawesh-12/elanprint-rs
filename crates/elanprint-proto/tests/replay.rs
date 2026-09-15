use elanprint_proto::{Enroll, EnrollAction, EnrollState};

struct Frame {
    dir: String,
    bytes: Vec<u8>,
}

fn fail(what: &str) -> ! {
    panic!("fixture error: {what}");
}

fn hex_decode(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for b in s.split_whitespace() {
        match u8::from_str_radix(b, 16) {
            Ok(v) => out.push(v),
            Err(_) => fail("fixture holds lowercase hex"),
        }
    }
    out
}

fn load_fixture(name: &str) -> Vec<Frame> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => fail("fixture exists"),
    };
    let raw: Vec<serde_json::Value> = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => fail("fixture is JSON"),
    };
    let mut frames = Vec::new();
    for f in &raw {
        let dir = match f.get("dir").and_then(|d| d.as_str()) {
            Some(d) => d.to_string(),
            None => fail("frame has dir"),
        };
        let bytes = match f.get("bytes").and_then(|b| b.as_str()) {
            Some(b) => hex_decode(b),
            None => fail("frame has bytes"),
        };
        frames.push(Frame { dir, bytes });
    }
    frames
}

#[test]
fn enroll_sequence_from_capture() {
    let frames = load_fixture("enroll_ok.json");
    assert!(!frames.is_empty(), "fixture holds at least one frame");
    assert_eq!(frames[0].dir, "out", "sequence starts with a send");

    let mut sm = Enroll::new(0);
    let EnrollAction::Send(first) = sm.start() else {
        panic!("start sends the first sample");
    };
    assert_eq!(first, frames[0].bytes, "first send matches the capture");

    let mut next_out = 1;
    let mut idx = 1;
    while idx < frames.len() {
        assert_eq!(frames[idx].dir, "in", "frame {idx} is a reply");
        match sm.step(&frames[idx].bytes) {
            EnrollAction::EmitProgress { .. } => {
                idx += 1;
                assert!(idx < frames.len(), "progress is followed by a send");
                assert_eq!(frames[idx].dir, "out", "frame {idx} is a send");
                let Some(EnrollAction::Send(bytes)) = sm.take_send() else {
                    panic!("progress is followed by a send");
                };
                assert_eq!(bytes, frames[idx].bytes, "send {next_out} matches");
                next_out += 1;
            }
            EnrollAction::Send(bytes) => {
                idx += 1;
                assert!(idx < frames.len(), "send is followed by its echo");
                assert_eq!(frames[idx].dir, "out", "frame {idx} is a send");
                assert_eq!(bytes, frames[idx].bytes, "send {next_out} matches");
                next_out += 1;
            }
            EnrollAction::Complete(id) => {
                assert_eq!(idx, frames.len() - 1, "complete comes from the last reply");
                assert_eq!(id, 0);
            }
            other => panic!("unexpected action at frame {idx}: {other:?}"),
        }
        idx += 1;
    }
    assert!(matches!(
        sm.state(),
        EnrollState::Done { template_id: 0 }
    ));
}
