use std::io::{self, Write};

use tsrs_lint::engine::{self, Options};
use tsrs_lint::payload::{Config, ConfiguredRule, Payload};

struct BrokenWriter;

impl Write for BrokenWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("injected output failure"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn repeated_library_calls_release_request_state_on_success_and_failure() {
    std::thread::Builder::new()
        .stack_size(512 * 1024 * 1024)
        .spawn(|| {
            let cwd = std::env::current_dir().unwrap().to_string_lossy().into_owned();
            let files = (0..16)
                .map(|index| format!("{cwd}/target/lint-lifecycle/virtual{index}.ts"))
                .collect::<Vec<_>>();
            let mut payload = Payload {
                configs: vec![Config {
                    file_paths: files.clone(),
                    rules: vec![ConfiguredRule {
                        name: "await-thenable".into(),
                        ..Default::default()
                    }],
                }],
                source_overrides: files
                    .into_iter()
                    .map(|file| (file, "export {}; await 123; const wrong: string = 1;".into()))
                    .collect(),
                report_semantic: false,
                ..Default::default()
            };
            for semantic in [false, true] {
                payload.report_semantic = semantic;
                for _ in 0..3 {
                    assert!(
                        engine::run(&payload, Options::default(), &cwd, &mut BrokenWriter).is_err()
                    );
                    let mut output = Vec::new();
                    engine::run(&payload, Options::default(), &cwd, &mut output).unwrap();
                    let mut count = 0;
                    let mut bytes = output.as_slice();
                    while !bytes.is_empty() {
                        let length = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
                        assert_eq!(bytes[4], 1);
                        bytes = &bytes[length + 5..];
                        count += 1;
                    }
                    assert_eq!(count, if semantic { 32 } else { 16 });
                }
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn repeated_library_calls_observe_changed_project_configuration() {
    std::thread::Builder::new()
        .stack_size(512 * 1024 * 1024)
        .spawn(|| {
            let cwd = std::env::current_dir().unwrap().to_string_lossy().into_owned();
            let base = format!("{cwd}/target/lint-config-cache-lifecycle");
            let file = format!("{base}/virtual.ts");
            let config = format!("{base}/tsconfig.json");
            let mut payload = Payload {
                configs: vec![Config {
                    file_paths: vec![file.clone()],
                    rules: Vec::new(),
                }],
                source_overrides: std::iter::once((file, "export {}; const value: number = null;".into()))
                    .collect(),
                report_semantic: true,
                ..Default::default()
            };
            for strict in [true, false, true, false] {
                payload.source_overrides.insert(
                    config.clone(),
                    format!(
                        r#"{{"compilerOptions":{{"strictNullChecks":{strict},"types":[]}},"files":["virtual.ts"]}}"#
                    ),
                );
                let mut output = Vec::new();
                engine::run(&payload, Options::default(), &cwd, &mut output).unwrap();
                if strict {
                    let length = u32::from_le_bytes(output[..4].try_into().unwrap()) as usize;
                    assert_eq!(output.len(), length + 5);
                    assert_eq!(output[4], 1);
                    assert!(std::str::from_utf8(&output[5..]).unwrap().contains("TS2322"));
                } else {
                    assert_eq!(output, [] as [u8; 0]);
                }
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn a_request_frees_each_program_before_the_next() {
    // The measure is how far chunks were ever carved into the arenas' address range, which plain pointers do not have.
    // Poisoning never uses freed memory again.
    if !tsrs_core::COMPRESSED_PTRS
        || std::env::var_os("TSRS_ARENA_POISON").is_some_and(|v| v == "1")
        || !crate::isolated::in_child("lifecycle::a_request_frees_each_program_before_the_next")
    {
        return;
    }
    std::thread::Builder::new()
        .stack_size(512 * 1024 * 1024)
        .spawn(|| {
            let cwd = std::env::current_dir().unwrap().to_string_lossy().into_owned();
            // Programs with nothing in common, each one large file: a request that keeps its programs carves it again.
            let source = format!("declare const n: number; n as number;{}", " n;".repeat(100_000));
            let config = r#"{"compilerOptions":{"noLib":true,"types":[]},"files":["index.ts"]}"#;
            let rule = "no-unnecessary-type-assertion";
            let lint = |projects| {
                let before = tsrs_core::ptr::reserve_stats().unwrap().1;
                let rules = vec![ConfiguredRule { name: rule.into(), ..Default::default() }];
                let mut payload = Payload::default();
                payload.configs.push(Config { rules, ..Default::default() });
                for project in 0..projects {
                    let base = format!("{cwd}/target/lint-program-memory-lifecycle/{project}");
                    payload.source_overrides.insert(format!("{base}/tsconfig.json"), config.into());
                    payload.source_overrides.insert(format!("{base}/index.ts"), source.clone());
                    payload.configs[0].file_paths.push(format!("{base}/index.ts"));
                }
                let mut output = Vec::new();
                engine::run(&payload, Options::default(), &cwd, &mut output).unwrap();
                let findings = String::from_utf8_lossy(&output).matches(rule).count();
                assert_eq!(findings, projects, "one finding per program");
                tsrs_core::ptr::reserve_stats().unwrap().1 - before
            };
            let two = lint(2);
            let twelve_more = lint(14);
            assert!(
                twelve_more < two,
                "14 programs carved {twelve_more} bytes more than 2 programs, which carved {two}"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}
