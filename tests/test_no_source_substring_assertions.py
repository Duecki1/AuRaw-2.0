from __future__ import annotations

import ast
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SOURCE_SUFFIXES = {
    ".c",
    ".cc",
    ".cpp",
    ".gradle",
    ".h",
    ".hpp",
    ".java",
    ".js",
    ".kt",
    ".kts",
    ".metal",
    ".py",
    ".rs",
    ".sh",
    ".ts",
    ".wgsl",
}
TARGETS = tuple((path, None) for path in sorted((ROOT / "tests").glob("test_*.py"))) + (
    (ROOT / "scripts/dev.py", {"run_cargo_test", "command_validate_math"}),
)


def names_written(node: ast.AST) -> set[str]:
    return {item.id for item in ast.walk(node) if isinstance(item, ast.Name)}


def contains_source_suffix(node: ast.AST) -> bool:
    return any(
        isinstance(item, ast.Constant)
        and isinstance(item.value, str)
        and Path(item.value).suffix.lower() in SOURCE_SUFFIXES
        for item in ast.walk(node)
    )


def depends_on(node: ast.AST, names: set[str]) -> bool:
    return any(isinstance(item, ast.Name) and item.id in names for item in ast.walk(node))


def is_text_read(node: ast.AST, source_paths: set[str]) -> bool:
    return (
        isinstance(node, ast.Call)
        and isinstance(node.func, ast.Attribute)
        and node.func.attr in {"read_text", "read_bytes"}
        and (
            contains_source_suffix(node.func.value)
            or depends_on(node.func.value, source_paths)
        )
    )


def source_content_names(tree: ast.AST) -> set[str]:
    source_paths: set[str] = set()
    source_contents: set[str] = set()
    assignments = [
        node
        for node in ast.walk(tree)
        if isinstance(node, (ast.Assign, ast.AnnAssign))
    ]

    changed = True
    while changed:
        changed = False
        for assignment in assignments:
            value = assignment.value
            targets = (
                assignment.targets
                if isinstance(assignment, ast.Assign)
                else [assignment.target]
            )
            target_names = set().union(*(names_written(target) for target in targets))
            if contains_source_suffix(value) or depends_on(value, source_paths):
                before = len(source_paths)
                source_paths.update(target_names)
                changed |= len(source_paths) != before
            if is_text_read(value, source_paths) or depends_on(value, source_contents):
                before = len(source_contents)
                source_contents.update(target_names)
                changed |= len(source_contents) != before
    return source_contents


def brittle_source_checks(
    path: Path, function_names: set[str] | None = None
) -> list[tuple[int, str]]:
    tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    if function_names is not None:
        selected = [
            node
            for node in tree.body
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
            and node.name in function_names
        ]
        assert {node.name for node in selected} == function_names
        tree = ast.Module(body=selected, type_ignores=[])

    contents = source_content_names(tree)
    issues: list[tuple[int, str]] = []
    for node in ast.walk(tree):
        if isinstance(node, ast.Compare) and any(
            isinstance(operator, (ast.In, ast.NotIn)) for operator in node.ops
        ):
            if depends_on(node, contents):
                issues.append((node.lineno, ast.unparse(node)))
        elif isinstance(node, ast.Call):
            is_contains = isinstance(node.func, ast.Attribute) and node.func.attr == "contains"
            is_regex = (
                isinstance(node.func, ast.Attribute)
                and isinstance(node.func.value, ast.Name)
                and node.func.value.id == "re"
                and node.func.attr in {"findall", "finditer", "fullmatch", "match", "search"}
            )
            if (is_contains or is_regex) and depends_on(node, contents):
                issues.append((node.lineno, ast.unparse(node)))
    return issues


def test_python_validators_do_not_assert_on_source_substrings() -> None:
    failures = {
        str(path.relative_to(ROOT)): brittle_source_checks(path, function_names)
        for path, function_names in TARGETS
        if brittle_source_checks(path, function_names)
    }
    assert not failures, failures
