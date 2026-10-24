#!/usr/bin/env python3
"""Flash vs Rust vs C++ benchmark driver.

Builds every benchmark with comparable optimisation settings, checks that all implementations print
the same output, and measures compile time, run time (median of N runs), peak memory and
executable size. Nothing is hard-coded: results are produced by running the programs.

    python3 benchmarks/run.py [--runs 5] [--only fib,sieve] [--flash path/to/flash] [--markdown out.md]
"""
import argparse, json, os, platform, shutil, statistics, subprocess, sys, time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
OUT = HERE / "out"

BENCHES = ["int_loop", "fib", "mandelbrot", "sieve", "strings", "alloc", "nbody", "matmul"]


RSSWRAP = None  # path of the rsswrap helper, set in main()


def build_rsswrap():
    cc = shutil.which("cc") or shutil.which("gcc") or shutil.which("clang")
    if not cc:
        return None
    OUT.mkdir(exist_ok=True)
    exe = OUT / "rsswrap"
    r = subprocess.run([cc, "-O1", str(HERE / "rsswrap.c"), "-o", str(exe)], capture_output=True)
    return str(exe) if r.returncode == 0 else None


def measure(cmd, runs):
    """Median wall time and peak RSS over `runs` executions.

    Peak memory (KiB) comes from the tiny `rsswrap` helper (see rsswrap.c) so that the harness's
    own memory is not inherited by the measurement; without a C compiler it is reported as 0.
    """
    times, rss, outs = [], [], set()
    for _ in range(runs):
        full = [RSSWRAP, *cmd] if RSSWRAP else list(cmd)
        t0 = time.perf_counter()
        p = subprocess.run(full, capture_output=True, env={**os.environ, "FLASH_PLUGIN_QUIET": "1"})
        times.append(time.perf_counter() - t0)
        err = p.stderr.decode().strip().splitlines()
        rss.append(int(err[-1]) if RSSWRAP and err and err[-1].isdigit() else 0)
        outs.add(p.stdout.decode().strip())
        if p.returncode != 0:
            outs.add("<exit %d>" % p.returncode)
    return statistics.median(times), min(times), max(rss), outs


def compile_timed(cmd):
    t0 = time.perf_counter()
    p = subprocess.run(cmd, capture_output=True, text=True)
    dt = time.perf_counter() - t0
    if p.returncode != 0:
        print("COMPILE FAILED:", " ".join(map(str, cmd)), "\n", p.stderr[-2000:], file=sys.stderr)
        return None
    return dt


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--runs", type=int, default=5)
    ap.add_argument("--only", default="")
    ap.add_argument("--flash", default=str(ROOT / "target" / "release" / "flash"))
    ap.add_argument("--markdown", default="")
    ap.add_argument("--json", default="")
    args = ap.parse_args()
    OUT.mkdir(exist_ok=True)
    global RSSWRAP
    RSSWRAP = build_rsswrap()
    benches = [b for b in BENCHES if not args.only or b in args.only.split(",")]
    cxx = shutil.which("g++") or shutil.which("c++") or shutil.which("clang++")
    rustc = shutil.which("rustc")
    results = []
    for b in benches:
        row = {"name": b, "impl": {}}
        variants = []
        variants.append(("flash-dev", [args.flash, "build", str(HERE / f"{b}.flash"), "-o", str(OUT / f"{b}_flash_dev")], OUT / f"{b}_flash_dev"))
        variants.append(("flash-release", [args.flash, "build", str(HERE / f"{b}.flash"), "--release", "-o", str(OUT / f"{b}_flash_rel")], OUT / f"{b}_flash_rel"))
        if shutil.which("clang") or os.environ.get("FLASH_CLANG"):
            variants.append(("flash-release-llvm", [args.flash, "build", str(HERE / f"{b}.flash"), "--release", "--backend", "llvm", "-o", str(OUT / f"{b}_flash_llvm")], OUT / f"{b}_flash_llvm"))
        if rustc:
            variants.append(("rust -O", [rustc, "-C", "opt-level=3", "-C", "strip=symbols", "--edition", "2021", str(HERE / f"{b}.rs"), "-o", str(OUT / f"{b}_rust")], OUT / f"{b}_rust"))
        if cxx:
            variants.append(("c++ -O3", [cxx, "-O3", "-std=c++17", "-s", str(HERE / f"{b}.cpp"), "-o", str(OUT / f"{b}_cpp")], OUT / f"{b}_cpp"))
            variants.append(("c++ -O0", [cxx, "-O0", "-std=c++17", str(HERE / f"{b}.cpp"), "-o", str(OUT / f"{b}_cpp0")], OUT / f"{b}_cpp0"))
        for name, cmd, exe in variants:
            ct = compile_timed(cmd)
            if ct is None:
                row["impl"][name] = None
                continue
            med, best, rss, outs = measure([str(exe)], args.runs)
            row["impl"][name] = {
                "compile_s": ct, "median_s": med, "best_s": best, "rss_kib": rss,
                "size_bytes": exe.stat().st_size, "output": sorted(outs),
            }
        # correctness: all outputs must agree with the C++ -O3 reference
        ref = (row["impl"].get("c++ -O3") or {}).get("output")
        for name, r in row["impl"].items():
            if r is not None:
                r["correct"] = (r["output"] == ref) if ref is not None else None
        results.append(row)
        print(f"done {b}", file=sys.stderr)

    info = {
        "machine": platform.platform(), "cpu": platform.processor() or "", "python": platform.python_version(),
        "rustc": subprocess.run([rustc, "--version"], capture_output=True, text=True).stdout.strip() if rustc else None,
        "cxx": subprocess.run([cxx, "--version"], capture_output=True, text=True).stdout.splitlines()[0] if cxx else None,
        "runs": args.runs, "date": time.strftime("%Y-%m-%d %H:%M:%S"),
    }
    try:
        for line in open("/proc/cpuinfo"):
            if line.startswith("model name"):
                info["cpu"] = line.split(":", 1)[1].strip()
                break
    except OSError:
        pass
    md = render_markdown(results, info)
    print(md)
    if args.markdown:
        Path(args.markdown).write_text(md)
    if args.json:
        Path(args.json).write_text(json.dumps({"info": info, "results": results}, indent=2))


def fmt_s(x):
    return f"{x:.3f}" if x < 10 else f"{x:.2f}"


def render_markdown(results, info):
    names = ["flash-dev", "flash-release", "flash-release-llvm", "rust -O", "c++ -O3", "c++ -O0"]
    out = []
    out.append("# Benchmark results (generated by benchmarks/run.py)\n")
    out.append(f"* Date: {info['date']}  \n* Machine: {info['machine']}  \n* CPU: {info['cpu']}  \n* Rust: {info['rustc']}  \n* C++: {info['cxx']}  \n* Runs per benchmark: {info['runs']} (median reported)\n")
    out.append("Run time = median wall-clock seconds of the compiled executable. Correct = output identical to the C++ -O3 build.\n")
    out.append("## Run time (seconds, lower is better)\n")
    out.append("| benchmark | " + " | ".join(names) + " |")
    out.append("|---|" + "---|" * len(names))
    for r in results:
        cells = []
        for n in names:
            x = r["impl"].get(n)
            if x is None:
                cells.append("n/a")
            else:
                cells.append(fmt_s(x["median_s"]) + ("" if x.get("correct") in (True, None) else " ✗WRONG"))
        out.append(f"| {r['name']} | " + " | ".join(cells) + " |")
    out.append("\n## Flash release vs Rust and C++ (run time ratio; 1.00 = same, >1 means Flash is slower; cranelift = `flash-release`, llvm = `flash-release-llvm`)\n")
    out.append("| benchmark | cranelift / rust | cranelift / c++ -O3 | llvm / rust | llvm / c++ -O3 |")
    out.append("|---|---|---|---|---|")
    for r in results:
        def ratio(mine, other):
            f = r["impl"].get(mine)
            o = r["impl"].get(other)
            return "n/a" if not f or not o else f"{f['median_s'] / o['median_s']:.2f}x"
        out.append(f"| {r['name']} | {ratio('flash-release', 'rust -O')} | {ratio('flash-release', 'c++ -O3')} | {ratio('flash-release-llvm', 'rust -O')} | {ratio('flash-release-llvm', 'c++ -O3')} |")
    out.append("\n## Compile time (seconds)\n")
    out.append("| benchmark | " + " | ".join(names) + " |")
    out.append("|---|" + "---|" * len(names))
    for r in results:
        out.append(f"| {r['name']} | " + " | ".join("n/a" if r["impl"].get(n) is None else f"{r['impl'][n]['compile_s']:.2f}" for n in names) + " |")
    out.append("\n## Peak memory (MiB, max RSS)\n")
    out.append("| benchmark | " + " | ".join(names) + " |")
    out.append("|---|" + "---|" * len(names))
    for r in results:
        out.append(f"| {r['name']} | " + " | ".join("n/a" if r["impl"].get(n) is None else f"{r['impl'][n]['rss_kib'] / 1024:.1f}" for n in names) + " |")
    out.append("\n## Executable size (KiB)\n")
    out.append("| benchmark | " + " | ".join(names) + " |")
    out.append("|---|" + "---|" * len(names))
    for r in results:
        out.append(f"| {r['name']} | " + " | ".join("n/a" if r["impl"].get(n) is None else f"{r['impl'][n]['size_bytes'] / 1024:.0f}" for n in names) + " |")
    out.append("")
    return "\n".join(out)


if __name__ == "__main__":
    main()
