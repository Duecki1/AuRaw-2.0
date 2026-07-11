from __future__ import annotations

from dataclasses import asdict, dataclass
import html
import json
from pathlib import Path
import xml.etree.ElementTree as ET


@dataclass(frozen=True)
class SceneResult:
    scene_id: str
    backend: str
    reference: str
    candidate: str
    metrics: dict[str, float]
    thresholds: dict[str, float]
    failures: tuple[str, ...]

    @property
    def passed(self) -> bool:
        return not self.failures


def evaluate_thresholds(metrics: dict[str, float], thresholds: dict[str, float]) -> tuple[str, ...]:
    failures: list[str] = []
    for name, limit in sorted(thresholds.items()):
        if name not in metrics:
            failures.append(f"metric {name!r} is not produced by the framework")
            continue
        value = metrics[name]
        if value > limit:
            failures.append(f"{name}={value:.8g} exceeds {limit:.8g}")
    return tuple(failures)


def write_json(path: Path | str, results: list[SceneResult], metadata: dict[str, object]) -> None:
    target = Path(path)
    target.parent.mkdir(parents=True, exist_ok=True)
    payload = {
        "schema": 1,
        "passed": all(result.passed for result in results),
        "metadata": metadata,
        "results": [asdict(result) | {"passed": result.passed} for result in results],
    }
    target.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def write_junit(path: Path | str, results: list[SceneResult]) -> None:
    target = Path(path)
    target.parent.mkdir(parents=True, exist_ok=True)
    suite = ET.Element(
        "testsuite",
        {
            "name": "auraw-image-regression",
            "tests": str(len(results)),
            "failures": str(sum(not result.passed for result in results)),
        },
    )
    for result in results:
        case = ET.SubElement(
            suite,
            "testcase",
            {"classname": f"image-regression.{result.backend}", "name": result.scene_id},
        )
        if result.failures:
            failure = ET.SubElement(case, "failure", {"message": result.failures[0]})
            failure.text = "\n".join(result.failures)
        out = ET.SubElement(case, "system-out")
        out.text = json.dumps(result.metrics, sort_keys=True)
    ET.ElementTree(suite).write(target, encoding="utf-8", xml_declaration=True)


def write_html(path: Path | str, results: list[SceneResult], metadata: dict[str, object]) -> None:
    target = Path(path)
    target.parent.mkdir(parents=True, exist_ok=True)
    metric_names = sorted({name for result in results for name in result.thresholds})
    rows: list[str] = []
    for result in results:
        cells = [
            f"<td>{html.escape(result.scene_id)}</td>",
            f"<td>{html.escape(result.backend)}</td>",
            f"<td>{'PASS' if result.passed else 'FAIL'}</td>",
        ]
        for name in metric_names:
            value = result.metrics.get(name)
            limit = result.thresholds.get(name)
            if value is None or limit is None:
                cells.append("<td>—</td>")
            else:
                bad = value > limit
                cells.append(
                    f"<td class={'bad' if bad else 'ok'}>{value:.5g}<br><small>≤ {limit:.5g}</small></td>"
                )
        cells.append(f"<td>{html.escape('; '.join(result.failures))}</td>")
        rows.append("<tr>" + "".join(cells) + "</tr>")
    headings = "".join(f"<th>{html.escape(name)}</th>" for name in metric_names)
    document = f"""<!doctype html>
<html><head><meta charset=\"utf-8\"><title>AuRaw image regression</title>
<style>body{{font-family:system-ui,sans-serif;margin:2rem}}table{{border-collapse:collapse;width:100%}}th,td{{border:1px solid #ccc;padding:.45rem;vertical-align:top}}th{{position:sticky;top:0;background:#fff}}.bad{{background:#ffd9d9}}.ok{{background:#e4f7e4}}small{{color:#555}}pre{{white-space:pre-wrap}}</style></head>
<body><h1>AuRaw image-quality regression</h1>
<pre>{html.escape(json.dumps(metadata, indent=2, sort_keys=True))}</pre>
<table><thead><tr><th>Scene</th><th>Backend</th><th>Status</th>{headings}<th>Failures</th></tr></thead>
<tbody>{''.join(rows)}</tbody></table></body></html>"""
    target.write_text(document, encoding="utf-8")
