use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

fn bench_expand_command_args(c: &mut Criterion) {
    let cmd = vec![
        "/opt/app/service".to_string(),
        r"--port=${SERVICE_PORT:-8900}".to_string(),
        r"--listen=${LISTEN_HOST:-127.0.0.1}".to_string(),
        r"--node=${HOME}/node-${SERVICE_PORT:-8900}".to_string(),
        r"--meta=$${literal}-${REGION:-${ZONE:-default}}".to_string(),
        r"--labels=${LABELS:-team=infra,role=service,env=prod}".to_string(),
    ];

    c.bench_function("resolve_command_args_expand_env", |b| {
        b.iter(|| {
            tino::bench_support::resolve_command_args(black_box(&cmd), true)
                .expect("expand env benchmark should succeed")
        })
    });
}

cfg_select! {
    target_os = "linux" => {
        fn bench_parse_shebang_interpreter(c: &mut Criterion) {
            let script = br#"#!/usr/bin/env -S python3 -u
print("hello")
"#;

            c.bench_function("parse_shebang_interpreter", |b| {
                b.iter(|| tino::bench_support::parse_shebang_interpreter(black_box(script)))
            });
        }

        fn bench_parse_elf_interpreter(c: &mut Criterion) {
            let elf = sample_elf_with_interp("/lib64/ld-linux-x86-64.so.2");

            c.bench_function("parse_elf_interpreter", |b| {
                b.iter(|| {
                    tino::bench_support::parse_elf_interpreter(black_box(&elf))
                        .expect("ELF interpreter benchmark should succeed")
                })
            });
        }

        fn sample_elf_with_interp(interpreter: &str) -> Vec<u8> {
            const ELF_HEADER_LEN: usize = 64;
            const PROGRAM_HEADER_LEN: usize = 56;
            const PT_INTERP: u32 = 3;

            let interp = format!("{interpreter}\0");
            let phoff = ELF_HEADER_LEN as u64;
            let interp_offset = (ELF_HEADER_LEN + PROGRAM_HEADER_LEN) as u64;
            let file_size = interp_offset as usize + interp.len();

            let mut bytes = vec![0u8; file_size];
            bytes[0..4].copy_from_slice(b"\x7FELF");
            bytes[4] = 2;
            bytes[5] = 1;
            bytes[6] = 1;
            bytes[16..18].copy_from_slice(&3u16.to_le_bytes());
            bytes[18..20].copy_from_slice(&62u16.to_le_bytes());
            bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
            bytes[32..40].copy_from_slice(&phoff.to_le_bytes());
            bytes[52..54].copy_from_slice(&(ELF_HEADER_LEN as u16).to_le_bytes());
            bytes[54..56].copy_from_slice(&(PROGRAM_HEADER_LEN as u16).to_le_bytes());
            bytes[56..58].copy_from_slice(&1u16.to_le_bytes());

            let program_header = ELF_HEADER_LEN;
            bytes[program_header..program_header + 4].copy_from_slice(&PT_INTERP.to_le_bytes());
            bytes[program_header + 8..program_header + 16]
                .copy_from_slice(&interp_offset.to_le_bytes());
            bytes[program_header + 32..program_header + 40]
                .copy_from_slice(&(interp.len() as u64).to_le_bytes());
            bytes[program_header + 40..program_header + 48]
                .copy_from_slice(&(interp.len() as u64).to_le_bytes());

            bytes[interp_offset as usize..].copy_from_slice(interp.as_bytes());
            bytes
        }
    }
    _ => {}
}

fn criterion_benches(c: &mut Criterion) {
    bench_expand_command_args(c);

    cfg_select! {
        target_os = "linux" => {
        bench_parse_shebang_interpreter(c);
        bench_parse_elf_interpreter(c);
        }
        _ => {}
    }
}

criterion_group!(benches, criterion_benches);
criterion_main!(benches);
