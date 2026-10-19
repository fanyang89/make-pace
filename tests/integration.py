#!/usr/bin/env python3
"""Real-tool smoke tests. Requires cargo, GNU Make >=4.4, CMake and Ninja >=1.13."""
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
BIN = ROOT / 'target/debug/make-pace'
env = dict(os.environ)
for key in ('MAKEFLAGS', 'MFLAGS', 'CARGO_MAKEFLAGS', 'CARGO_BUILD_JOBS', 'CMAKE_BUILD_PARALLEL_LEVEL'):
    env.pop(key, None)
base = Path(env.get('TMPDIR', str(Path.home() / 'tmp'))) / 'pi'
base.mkdir(parents=True, exist_ok=True)


def run(args, cwd, code=0):
    result = subprocess.run([str(BIN), '--', *args], cwd=cwd, env=env,
                            text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=90)
    assert result.returncode == code, result.stdout + result.stderr
    assert 'jobserver unavailable' not in result.stderr.lower(), result.stderr
    assert 'ignoring jobserver' not in result.stderr.lower(), result.stderr
    print('PASS', ' '.join(args))
    return result


with tempfile.TemporaryDirectory(prefix='resource-tests-', dir=base) as temporary:
    root = Path(temporary)
    makefile = root / 'Makefile'
    makefile.write_text('.PHONY: all ' + ' '.join(f't{i}' for i in range(12)) + '\n'
                        + 'all: ' + ' '.join(f't{i}' for i in range(12)) + '\n'
                        + ''.join(f't{i}:\n\t@sleep 0.4\n' for i in range(12))
                        + 'fail:\n\t@exit 1\n'
                        + 'long:\n\t@sleep 60\n'
                        + 'nested:\n\t+$(MAKE) all\n')
    run(['make'], root)
    run(['make', 'nested'], root)
    run(['make', 'fail'], root, 2)
    run(['make', '-j4'], root, 1)
    # A parent owns the token pool; the wrapper must not replace it.
    makefile.write_text(makefile.read_text() + f'parent:\n\t+{BIN} -- make all\n')
    result = subprocess.run(['make', '-j2', 'parent'], cwd=root, env=env,
                            capture_output=True, text=True, timeout=60)
    assert result.returncode == 0, result.stderr
    assert 'inherited jobserver' in result.stderr, result.stderr
    print('PASS inherited parent jobserver')
    makefile.write_text(makefile.read_text() +
        f"cargo_parent:\n\t+env -u MAKEFLAGS -u MFLAGS CARGO_MAKEFLAGS='$(MAKEFLAGS)' {BIN} -- make all\n")
    result = subprocess.run(['make', '-j2', 'cargo_parent'], cwd=root, env=env,
                            capture_output=True, text=True, timeout=60)
    assert result.returncode == 0, result.stderr
    assert 'inherited jobserver' in result.stderr, result.stderr
    assert 'jobserver unavailable' not in result.stderr.lower(), result.stderr
    print('PASS Cargo-style inherited flags')

    (root / 'CMakeLists.txt').write_text('cmake_minimum_required(VERSION 3.16)\n'
        'project(smoke NONE)\n' + ''.join(
            f'add_custom_target(task{i} ALL COMMAND ${{CMAKE_COMMAND}} -E sleep 0.4)\n'
            for i in range(12)))
    subprocess.run(['cmake', '-S', str(root), '-B', str(root / 'build'), '-G', 'Ninja'],
                   env=env, check=True, capture_output=True, timeout=30)
    run(['cmake', '--build', 'build'], root)
    run(['ninja', '-C', 'build'], root)
    makefile.write_text(makefile.read_text() + f'pipe_parent:\n\t+{BIN} -- ninja -C build\n')
    result = subprocess.run(['make', '--jobserver-style=pipe', '-j2', 'pipe_parent'],
                            cwd=root, env=env, capture_output=True, text=True, timeout=30)
    assert result.returncode != 0 and 'Ninja requires an inherited FIFO' in result.stderr, result.stderr
    print('PASS incompatible inherited protocol rejection')

    (root / 'Cargo.toml').write_text('[package]\nname="smoke"\nversion="0.1.0"\nedition="2024"\n')
    (root / 'src').mkdir()
    (root / 'src/main.rs').write_text('fn main() {}\n')
    (root / 'build.rs').write_text('fn main() {\n'
        'let flags = std::env::var("CARGO_MAKEFLAGS").unwrap();\n'
        'assert!(flags.contains("--jobserver-auth=fifo:"));\n'
        'assert!(std::process::Command::new("make").arg("all")\n'
        '.env("MAKEFLAGS", flags).status().unwrap().success());\n}\n')
    run(['cargo', 'build', '--offline'], root)

    proc = subprocess.Popen([str(BIN), '--', 'make', 'long'], cwd=root, env=env,
                            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
    time.sleep(0.5)
    proc.send_signal(signal.SIGTERM)
    _, stderr = proc.communicate(timeout=10)
    assert proc.returncode != 0, stderr
    print('PASS signal forwarding')
