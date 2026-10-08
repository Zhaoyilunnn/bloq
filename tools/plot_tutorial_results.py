"""Replot retained tutorial measurements; no sampling or model fitting.

Run with Matplotlib and NumPy installed. Inputs and source hashes are retained in
docs/assets/experiments/tutorial-results.json, so the external experiment checkout
is not needed. All plotted curves stop within the measured parameter ranges.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib.lines import Line2D
import numpy as np

ROOT = Path(__file__).resolve().parents[1]
COLORS = ("#0072B2", "#D55E00", "#009E73", "#CC79A7", "#806600")
MARKERS = ("o", "s", "^", "D", "v")


def bars(ax, x, y, bounds, *, color, marker, filled=True, label=None, linestyle="none"):
    bounds = np.asarray(bounds)
    ax.errorbar(
        x, y, yerr=[np.maximum(0, y - bounds[:, 0]), np.maximum(0, bounds[:, 1] - y)],
        color=color, marker=marker, ms=3.5, mfc=color if filled else "white",
        linestyle=linestyle, lw=0.8, elinewidth=0.7, capsize=1.5, label=label,
    )


def cnot_figures(data, output):
    for observable in data["observables"]:
        fig, ax = plt.subplots(figsize=(6.4, 4.2), layout="constrained")
        for d, color, marker in zip((3, 5, 7, 9, 11), COLORS, MARKERS, strict=True):
            rows = sorted((r for r in observable["points"] if r["distance"] == d),
                          key=lambda r: r["physical_error_probability"])
            positive = [r for r in rows if r["errors"] > 0]
            x = np.array([r["physical_error_probability"] for r in positive])
            y = np.array([r["logical_error_rate"] for r in positive])
            bounds = [[r["ci95_low"], r["ci95_high"]] for r in positive]
            bars(ax, x, y, bounds, color=color, marker=marker,
                 linestyle="-", label=f"d = {d}")
            for r in rows:
                if r["errors"] == 0:
                    ax.errorbar(r["physical_error_probability"], r["ci95_high"],
                                yerr=r["ci95_high"] * 0.3, uplims=True, color=color,
                                fmt=marker, ms=3.5, capsize=1.5)
        ax.set(xscale="log", yscale="log", xlim=(1e-4, 0.1), ylim=(1e-8, 1),
               xlabel="Physical error probability p", ylabel="Logical error probability",
               title="$" + observable["correlation_surface"].replace("->", r"\to") + "$")
        ax.grid(which="major", alpha=0.18, lw=0.5)
        ax.legend(frameon=False, fontsize=8, loc="lower right")
        path = output / f"cnot-fill{observable['fill']}-{observable['observable']}.svg"
        fig.savefig(path)
        path.write_text("\n".join(line.rstrip() for line in path.read_text().splitlines()) + "\n")
        plt.close(fig)


def t_source_figure(data, output):
    fig, ax = plt.subplots(figsize=(7.5, 5.4), layout="constrained")
    probabilities = sorted({r["physical_error_probability"] for r in data["points"]})
    distances = (5, 7, 9, 11)
    styles = ("-", "--", "-.", ":")
    for p, color in zip(probabilities, COLORS, strict=True):
        for d, marker, style in zip(distances, MARKERS, styles, strict=False):
            rows = sorted((r for r in data["points"] if r["distance"] == d
                           and r["physical_error_probability"] == p
                           and r["infidelity"] is not None and r["infidelity"] > 0),
                          key=lambda r: r["attempted_acceptance"])
            x = np.array([r["attempted_acceptance"] for r in rows])
            y = np.array([r["infidelity"] for r in rows])
            bounds = [[max(0, r["infidelity"] - r["ci95_half_width"]),
                       r["infidelity"] + r["ci95_half_width"]] for r in rows]
            bars(ax, x, y, bounds, color=color, marker=marker, linestyle=style)
    ax.set(yscale="log", xlim=(0, 1), xlabel="Source acceptance probability",
           ylabel=r"Accepted $|T\rangle$ infidelity")
    ax.grid(which="major", alpha=0.18, lw=0.5)
    physical = ax.legend(
        handles=[Line2D([], [], color=c, lw=1.5, label=f"p = {p:g}")
                 for p, c in zip(probabilities, COLORS, strict=True)],
        loc="upper right", frameon=False, fontsize=8, ncols=2,
    )
    ax.add_artist(physical)
    ax.legend(handles=[Line2D([], [], color="0.3", marker=m, ls=s, ms=3.5,
                              label=f"d = {d}")
                       for d, m, s in zip(distances, MARKERS, styles, strict=False)],
              loc="upper right", bbox_to_anchor=(1, 0.82),
              frameon=False, fontsize=8, ncols=2)
    fig.savefig(output / "t-cultivation-multi-p.svg")
    plt.close(fig)


def t_gate_figure(data, output):
    states = ("0", "1", "+", "-", "+i", "-i", "avg", "choi")
    fig, ax = plt.subplots(figsize=(7.5, 4.2), layout="constrained")
    for offset, (policy, color, label) in enumerate((
        ("eval", COLORS[0], "Physical cultivated source"),
        ("t-proxy", COLORS[1], "Calibrated source proxy"),
    )):
        rows = {r["target"]: r for r in data["points"] if r["policy"] == policy}
        inputs = [rows[state] for state in states[:6]]
        rows["avg"] = {
            "infidelity": np.mean([r["infidelity"] for r in inputs]),
            "ci95_half_width": np.sqrt(sum(r["ci95_half_width"] ** 2 for r in inputs)) / 6,
        }
        ax.errorbar(
            np.arange(len(states)) + (offset - 0.5) * 0.18,
            [rows[state]["infidelity"] for state in states],
            yerr=[rows[state]["ci95_half_width"] for state in states],
            color=color, marker="o", mfc=color if offset == 0 else "white",
            linestyle="none", ms=4, capsize=2, label=label,
        )
    ax.set(xticks=np.arange(len(states)),
           xticklabels=[f"$|{s}\\rangle$" for s in states[:6]] + ["Six-state\nmean", "Choi"],
           ylabel="Accepted output infidelity", ylim=(0, 7e-5),
           title=r"Logical T gate: $d=9$, $p=10^{-3}$")
    ax.ticklabel_format(axis="y", style="sci", scilimits=(0, 0))
    ax.grid(axis="y", alpha=0.18, lw=0.5)
    ax.legend(frameon=False, fontsize=8, loc="upper left")
    fig.savefig(output / "t-gate-characterization.svg")
    plt.close(fig)


def ccz_figure(data, output):
    fig, axes = plt.subplots(2, 2, figsize=(8.1, 6.4), layout="constrained")
    distances = (9, 11, 13, 15)
    factories = ("non-tels", "tels")
    qmin = min(r["t_infidelity"] for r in data["points"] if r["t_infidelity"] > 0)
    qmax = max(r["t_infidelity"] for r in data["points"])
    qgrid = np.geomspace(qmin, qmax, 160)
    # Exact independent-Z source reference from the retained [8,4,4] code model.
    masses = np.array([qgrid**k * (1 - qgrid)**(8 - k) for k in range(9)])
    acceptance = np.array([1, 0, 28, 0, 70, 0, 28, 0, 1]) @ masses
    source = (np.array([0, 0, 28, 0, 56, 0, 28, 0, 0]) @ masses) / acceptance
    for column, factory in enumerate(factories):
        ax = axes[0, column]
        for d, color, marker in zip(distances, COLORS, MARKERS, strict=False):
            rows = sorted((r for r in data["points"] if r["factory"] == factory
                           and r["distance"] == d), key=lambda r: r["t_infidelity"])
            floor = next(r for r in rows if r["t_infidelity"] == 0)
            rows = [r for r in rows if r["t_infidelity"] > 0]
            x = np.array([r["t_infidelity"] for r in rows])
            y = np.array([r["infidelity"] for r in rows])
            bounds = [r["ci95"] for r in rows]
            bars(ax, x, y, bounds, color=color, marker=marker,
                 filled=factory == "non-tels")
            ax.plot(qgrid, source + floor["infidelity"], "--", color=color, lw=0.8)
            ax.fill_between(qgrid, source + floor["ci95"][0], source + floor["ci95"][1],
                            color=color, alpha=0.08, lw=0)
        ax.plot(qgrid, source, color="0.2", lw=1.0)
        ax.set(xscale="log", yscale="log", xlabel="Input T-state error probability q",
               ylabel="Accepted output infidelity" if column == 0 else "",
               title=f"({'ab'[column]}) {'Conventional' if column == 0 else 'TELS'}")
        ax.grid(which="major", alpha=0.18, lw=0.5)
    axes[0, 0].legend(handles=[Line2D([], [], color="0.2", label="Source only S(q)"),
                              Line2D([], [], color="0.4", ls="--", label="C(d) + S(q)")],
                      loc="lower right", frameon=False, fontsize=7)

    ax = axes[1, 0]
    for factory in factories:
        fit = next(f for f in data["floor_models"] if f["factory"] == factory)
        ds = np.linspace(9, 15, 100)
        vector = np.array([np.ones_like(ds), -(ds - 9) / 2]).T
        params = [fit["parameters"]["log_amplitude"], fit["parameters"]["decay"]]
        log_y = vector @ np.array(params)
        covariance = np.array(fit["parameter_covariance"])
        sigma = np.sqrt(np.maximum(0, np.einsum("ij,jk,ik->i", vector, covariance, vector)))
        shade = "0.2" if factory == "non-tels" else "0.55"
        ax.plot(ds, np.exp(log_y), color=shade, ls="-" if factory == "non-tels" else "--")
        ax.fill_between(ds, np.exp(log_y - 1.96 * sigma), np.exp(log_y + 1.96 * sigma),
                        color=shade, alpha=0.10, lw=0)
        for d, color, marker in zip(distances, COLORS, MARKERS, strict=False):
            row = next(r for r in data["points"] if r["factory"] == factory
                       and r["distance"] == d and r["t_infidelity"] == 0)
            bars(ax, np.array([d]), np.array([row["infidelity"]]), [row["ci95"]],
                 color=color, marker=marker, filled=factory == "non-tels")
    ax.axvspan(13.15, 15.35, color="0.94", zorder=-1)
    ax.text(14.2, 3e-4, "Held out", ha="center", fontsize=7, color="0.4")
    ax.set(yscale="log", xticks=distances, xlim=(8.6, 15.4),
           xlabel="Code distance d", ylabel="Circuit floor C(d)", title="(c) Circuit suppression")
    ax.grid(which="major", alpha=0.18, lw=0.5)

    ax = axes[1, 1]
    for factory in factories:
        for d, color, marker in zip(distances, COLORS, MARKERS, strict=False):
            rows = sorted((r for r in data["points"] if r["factory"] == factory
                           and r["distance"] == d and r["t_infidelity"] > 0),
                          key=lambda r: r["t_infidelity"])
            x = np.array([r["t_infidelity"] for r in rows])
            y = np.array([1 - r["factory_acceptance"] for r in rows])
            bounds = [[1 - r["factory_acceptance_ci95_nominal"][1],
                       1 - r["factory_acceptance_ci95_nominal"][0]] for r in rows]
            bars(ax, x, y, bounds, color=color, marker=marker,
                 filled=factory == "non-tels", linestyle="-" if factory == "non-tels" else "--")
    ax.set(xscale="log", yscale="log", xlabel="Input T-state error probability q",
           ylabel="Factory discard probability", title="(d) Discard rate")
    ax.grid(which="major", alpha=0.18, lw=0.5)
    fig.legend(handles=[Line2D([], [], color=c, marker=m, ls="none", label=f"d = {d}")
                        for d, c, m in zip(distances, COLORS, MARKERS, strict=False)]
               + [Line2D([], [], color="0.3", marker="o", ls="-", label="Conventional"),
                  Line2D([], [], color="0.3", marker="o", mfc="white", ls="--", label="TELS")],
               loc="outside upper center", ncols=6, frameon=False, fontsize=8)
    fig.savefig(output / "ccz-factory-response.svg")
    plt.close(fig)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", type=Path, default=ROOT / "docs/assets/experiments")
    args = parser.parse_args()
    data = json.loads((ROOT / "docs/assets/experiments/tutorial-results.json").read_text())
    # Guard the retained coverage and population: no guessed or pooled rows.
    assert data["schema"] == 1 and len(data["cnot"]["observables"]) == 4
    assert sum(len(o["points"]) for o in data["cnot"]["observables"]) == 340
    assert len(data["t_source"]["points"]) == 480 and len(data["ccz"]["points"]) == 50
    assert all(r["estimator"] == "unpinned-path-mixture"
               and r["physical_error_probability"] == 0.001 for r in data["ccz"]["points"])
    plt.rcParams.update({"font.family": "DejaVu Sans", "font.size": 9,
                         "axes.spines.top": False, "axes.spines.right": False,
                         "figure.facecolor": "white", "axes.facecolor": "white",
                         "savefig.facecolor": "white", "savefig.transparent": False,
                         "svg.fonttype": "none"})
    args.output_dir.mkdir(parents=True, exist_ok=True)
    cnot_figures(data["cnot"], args.output_dir)
    t_source_figure(data["t_source"], args.output_dir)
    t_gate_figure(data["t_gate"], args.output_dir)
    ccz_figure(data["ccz"], args.output_dir)


if __name__ == "__main__":
    main()
