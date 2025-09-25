#!/usr/bin/env python3
"""
Performance requirements validator for Tokio-Pulse
Ensures that critical operations remain under specified thresholds
"""

import json
import os
import sys
from pathlib import Path

# Performance requirements (nanoseconds)
REQUIREMENTS = {
    # Hook operations must be < 100ns
    "hook_registry/no_hooks/before_poll": 100,
    "hook_registry/no_hooks/after_poll": 100,
    "hook_registry/with_hooks/before_poll": 100,
    "hook_registry/with_hooks/after_poll": 100,

    # Budget operations must be < 50ns
    "task_budget/operations/consume": 50,
    "task_budget/operations/is_exhausted": 20,
    "task_budget/operations/remaining": 20,

    # CPU timing must be < 50ns
    "cpu_timer/single_measurement": 50,

    # Critical path (complete poll cycle) must be < 150ns
    "critical_path/poll_cycle/no_hooks": 150,
    "critical_path/poll_cycle/with_hooks": 200,
}

def parse_criterion_output(criterion_dir):
    """Parse Criterion benchmark results"""
    results = {}

    for bench_dir in Path(criterion_dir).glob("*/*/base"):
        bench_name = bench_dir.parent.name
        estimates_file = bench_dir / "estimates.json"

        if estimates_file.exists():
            with open(estimates_file) as f:
                data = json.load(f)
                # Criterion reports in picoseconds, convert to nanoseconds
                mean_ns = data["mean"]["point_estimate"] / 1000
                results[bench_name] = mean_ns

    return results

def check_requirements(results):
    """Check if benchmark results meet requirements"""
    violations = []

    for bench_name, threshold_ns in REQUIREMENTS.items():
        if bench_name in results:
            actual_ns = results[bench_name]
            if actual_ns > threshold_ns:
                violations.append({
                    "benchmark": bench_name,
                    "threshold": threshold_ns,
                    "actual": actual_ns,
                    "excess": actual_ns - threshold_ns,
                    "percent": ((actual_ns / threshold_ns) - 1) * 100
                })

    return violations

def main():
    criterion_dir = Path("target/criterion")

    if not criterion_dir.exists():
        print("No benchmark results found. Run 'cargo bench' first.")
        sys.exit(1)

    results = parse_criterion_output(criterion_dir)
    violations = check_requirements(results)

    if violations:
        print("❌ Performance requirements violated:\n")
        for v in violations:
            print(f"  {v['benchmark']}:")
            print(f"    Threshold: {v['threshold']:.1f}ns")
            print(f"    Actual:    {v['actual']:.1f}ns")
            print(f"    Exceeded by: {v['excess']:.1f}ns ({v['percent']:.1f}%)\n")

        sys.exit(1)
    else:
        print("✅ All performance requirements met!")
        print("\nKey metrics:")
        for bench_name, threshold_ns in REQUIREMENTS.items():
            if bench_name in results:
                actual_ns = results[bench_name]
                margin = threshold_ns - actual_ns
                percent = (margin / threshold_ns) * 100
                print(f"  {bench_name}: {actual_ns:.1f}ns (margin: {margin:.1f}ns, {percent:.1f}%)")

if __name__ == "__main__":
    main()