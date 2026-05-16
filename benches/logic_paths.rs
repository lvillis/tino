use std::hint::black_box;
use std::time::{Duration, Instant};

const MIN_SAMPLE_TIME: Duration = Duration::from_millis(200);
const MAX_ITERS: u64 = 1 << 32;

fn main() {
    bench_expand_command_args();

    cfg_select! {
        target_os = "linux" => {
            bench_parse_shebang_interpreter();
            bench_parse_elf_interpreter();
        }
        _ => {}
    }
}

fn bench_expand_command_args() {
    let cmd = vec![
        "/opt/app/service".to_string(),
        r"--port=${SERVICE_PORT:-8900}".to_string(),
        r"--listen=${LISTEN_HOST:-127.0.0.1}".to_string(),
        r"--node=${APP_HOME:-/opt/app}/node-${SERVICE_PORT:-8900}".to_string(),
        r"--meta=$${literal}-${REGION:-${ZONE:-default}}".to_string(),
        r"--labels=${LABELS:-team=infra,role=service,env=prod}".to_string(),
    ];

    bench("resolve_command_args_expand_env", || {
        tino::bench_support::resolve_command_args(black_box(&cmd), true)
            .expect("expand env benchmark should succeed")
    });
}

cfg_select! {
    target_os = "linux" => {
        fn bench_parse_shebang_interpreter() {
            let script = br#"#!/usr/bin/env -S python3 -u
print("hello")
"#;

            bench("parse_shebang_interpreter", || {
                tino::bench_support::parse_shebang_interpreter(black_box(script))
            });
        }

        fn bench_parse_elf_interpreter() {
            let elf = sample_elf_with_interp("/lib64/ld-linux-x86-64.so.2");

            bench("parse_elf_interpreter", || {
                tino::bench_support::parse_elf_interpreter(black_box(&elf))
                    .expect("ELF interpreter benchmark should succeed")
            });
        }

        fn sample_elf_with_interp(interpreter: &str) -> Vec<u8> {
            const ELF_HEADER_LEN: usize = 64;
            const PROGRAM_HEADER_LEN: usize = 56;
            const PT_INTERP: u32 = 3;

            let interp = format!("{interpreter}\0");
            let phoff = u64::try_from(ELF_HEADER_LEN).expect("ELF header length should fit u64");
            let interp_offset = u64::try_from(ELF_HEADER_LEN + PROGRAM_HEADER_LEN)
                .expect("sample ELF offset should fit u64");
            let file_size = usize::try_from(interp_offset)
                .expect("sample ELF offset should fit usize")
                + interp.len();
            let interp_len =
                u64::try_from(interp.len()).expect("sample interpreter length should fit u64");

            let mut bytes = vec![0u8; file_size];
            bytes[0..4].copy_from_slice(b"\x7FELF");
            bytes[4] = 2;
            bytes[5] = 1;
            bytes[6] = 1;
            bytes[16..18].copy_from_slice(&3u16.to_le_bytes());
            bytes[18..20].copy_from_slice(&62u16.to_le_bytes());
            bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
            bytes[32..40].copy_from_slice(&phoff.to_le_bytes());
            bytes[52..54].copy_from_slice(
                &u16::try_from(ELF_HEADER_LEN)
                    .expect("ELF header length should fit u16")
                    .to_le_bytes(),
            );
            bytes[54..56].copy_from_slice(
                &u16::try_from(PROGRAM_HEADER_LEN)
                    .expect("program header length should fit u16")
                    .to_le_bytes(),
            );
            bytes[56..58].copy_from_slice(&1u16.to_le_bytes());

            let program_header = ELF_HEADER_LEN;
            bytes[program_header..program_header + 4].copy_from_slice(&PT_INTERP.to_le_bytes());
            bytes[program_header + 8..program_header + 16]
                .copy_from_slice(&interp_offset.to_le_bytes());
            bytes[program_header + 32..program_header + 40]
                .copy_from_slice(&interp_len.to_le_bytes());
            bytes[program_header + 40..program_header + 48]
                .copy_from_slice(&interp_len.to_le_bytes());

            let interp_offset =
                usize::try_from(interp_offset).expect("sample ELF offset should fit usize");
            bytes[interp_offset..].copy_from_slice(interp.as_bytes());
            bytes
        }
    }
    _ => {}
}

fn bench<T>(name: &str, mut f: impl FnMut() -> T) {
    let mut iters = 1u64;

    loop {
        let started = Instant::now();
        for _ in 0..iters {
            black_box(f());
        }
        let elapsed = started.elapsed();
        if elapsed >= MIN_SAMPLE_TIME || iters >= MAX_ITERS {
            report(name, iters, elapsed);
            break;
        }
        iters = iters.saturating_mul(2).min(MAX_ITERS);
    }
}

fn report(name: &str, iters: u64, elapsed: Duration) {
    let nanos_per_iter = elapsed.as_nanos() / u128::from(iters);
    println!("{name}: {nanos_per_iter} ns/iter ({iters} iterations in {elapsed:.3?})");
}
