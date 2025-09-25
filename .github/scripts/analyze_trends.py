#!/usr/bin/env python3
"""
Analyze performance trends over time
Detect regressions and improvements
"""

import json
import os
from datetime import datetime
from pathlib import Path
import statistics

def load_historical_data():
    """Load benchmark data from all nightly runs"""
    data = []

    for artifact in Path(".").glob("nightly-bench-*"):
        date_str = artifact.name.split("-")[-1]
        criterion_dir = artifact / "criterion"

        if criterion_dir.exists():
            results = {}
            for bench_dir in criterion_dir.glob("*/*/base"):
                bench_name = bench_dir.parent.name
                estimates_file = bench_dir / "estimates.json"

                if estimates_file.exists():
                    with open(estimates_file) as f:
                        estimates = json.load(f)
                        results[bench_name] = estimates["mean"]["point_estimate"] / 1000

            data.append({
                "date": date_str,
                "results": results
            })

    return sorted(data, key=lambda x: x["date"])

def detect_regressions(data, threshold=0.05):
    """Detect performance regressions (>5% increase)"""
    regressions = []

    if len(data) < 2:
        return regressions

    latest = data[-1]["results"]
    previous = data[-2]["results"]

    for bench_name in latest:
        if bench_name in previous:
            current = latest[bench_name]
            prev = previous[bench_name]
            change = (current - prev) / prev

            if change > threshold:
                regressions.append({
                    "benchmark": bench_name,
                    "previous": prev,
                    "current": current,
                    "change_percent": change * 100
                })

    return regressions

def calculate_statistics(data):
    """Calculate statistics for each benchmark"""
    stats = {}

    # Collect all values per benchmark
    by_benchmark = {}
    for entry in data:
        for bench_name, value in entry["results"].items():
            if bench_name not in by_benchmark:
                by_benchmark[bench_name] = []
            by_benchmark[bench_name].append(value)

    # Calculate statistics
    for bench_name, values in by_benchmark.items():
        if len(values) >= 2:
            stats[bench_name] = {
                "mean": statistics.mean(values),
                "median": statistics.median(values),
                "stdev": statistics.stdev(values) if len(values) > 2 else 0,
                "min": min(values),
                "max": max(values),
                "latest": values[-1],
                "trend": "stable"
            }

            # Determine trend
            if len(values) >= 5:
                recent = values[-5:]
                older = values[-10:-5] if len(values) >= 10 else values[:len(values)-5]

                recent_mean = statistics.mean(recent)
                older_mean = statistics.mean(older) if older else recent_mean

                change = (recent_mean - older_mean) / older_mean if older_mean else 0

                if change > 0.02:
                    stats[bench_name]["trend"] = "degrading"
                elif change < -0.02:
                    stats[bench_name]["trend"] = "improving"

    return stats

def generate_html_report(data, regressions, stats):
    """Generate HTML report with trends"""
    html = ["""
<!DOCTYPE html>
<html>
<head>
    <title>Tokio-Pulse Performance Trends</title>
    <style>
        body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; margin: 40px; }
        h1 { color: #333; }
        .regression { background: #fee; padding: 15px; border-left: 4px solid #f00; margin: 20px 0; }
        .improvement { background: #efe; padding: 15px; border-left: 4px solid #0a0; margin: 20px 0; }
        .stable { background: #f9f9f9; padding: 15px; border-left: 4px solid #ccc; margin: 20px 0; }
        table { border-collapse: collapse; width: 100%; margin: 20px 0; }
        th, td { border: 1px solid #ddd; padding: 12px; text-align: left; }
        th { background: #f5f5f5; font-weight: 600; }
        .trend-up { color: #d00; }
        .trend-down { color: #0a0; }
        .metric { font-family: 'SF Mono', Monaco, monospace; }
    </style>
</head>
<body>
    <h1>Tokio-Pulse Performance Trends</h1>
    <p>Generated: """ + datetime.now().isoformat() + "</p>"]

    # Regressions section
    if regressions:
        html.append("<h2>⚠️ Performance Regressions Detected</h2>")
        for r in regressions:
            html.append(f"""
            <div class="regression">
                <strong>{r['benchmark']}</strong><br>
                Previous: {r['previous']:.2f}ns<br>
                Current: {r['current']:.2f}ns<br>
                Change: <span class="trend-up">+{r['change_percent']:.1f}%</span>
            </div>
            """)
    else:
        html.append("<h2>✅ No Regressions Detected</h2>")

    # Statistics table
    html.append("<h2>Performance Statistics</h2>")
    html.append("<table>")
    html.append("<tr><th>Benchmark</th><th>Latest</th><th>Mean</th><th>StdDev</th><th>Min</th><th>Max</th><th>Trend</th></tr>")

    for bench_name, s in sorted(stats.items()):
        trend_class = ""
        trend_symbol = "→"
        if s["trend"] == "degrading":
            trend_class = "trend-up"
            trend_symbol = "↑"
        elif s["trend"] == "improving":
            trend_class = "trend-down"
            trend_symbol = "↓"

        html.append(f"""
        <tr>
            <td class="metric">{bench_name}</td>
            <td>{s['latest']:.2f}ns</td>
            <td>{s['mean']:.2f}ns</td>
            <td>{s['stdev']:.2f}</td>
            <td>{s['min']:.2f}ns</td>
            <td>{s['max']:.2f}ns</td>
            <td class="{trend_class}">{trend_symbol} {s['trend']}</td>
        </tr>
        """)

    html.append("</table>")
    html.append("</body></html>")

    return "\n".join(html)

def main():
    data = load_historical_data()

    if not data:
        print("No historical data found")
        return

    regressions = detect_regressions(data)
    stats = calculate_statistics(data)
    html = generate_html_report(data, regressions, stats)

    with open("performance-trends.html", "w") as f:
        f.write(html)

    print(f"Performance trends analyzed: {len(stats)} benchmarks")
    if regressions:
        print(f"WARNING: {len(regressions)} regressions detected!")
        for r in regressions:
            print(f"  - {r['benchmark']}: +{r['change_percent']:.1f}%")
        exit(1)

if __name__ == "__main__":
    main()