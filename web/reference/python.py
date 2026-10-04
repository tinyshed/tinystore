"""Print a Python SDK engine's public API as JSON for the site's API pages.

    python reference/python.py <package dir> <module>...

Reads the source with ast and imports nothing, so the SDK's dependencies need
not be installed. A name is public when the package's __all__ exports it or
the Store class keeps an instance of it, as store.kv keeps a Kv. Each item has
the shape web/reference/go prints: its code without bodies, its docstring as
markdown, and its members.
"""

from __future__ import annotations

import ast
import copy
import json
import re
import sys
from pathlib import Path


def main() -> None:
    package, modules = Path(sys.argv[1]), sys.argv[2:]
    public = exported(package) | kept_by_store(package)
    items = []
    for module in modules:
        tree = ast.parse((package / f"{module}.py").read_text(encoding="utf-8"))
        items.extend(item(node) for node in tree.body if named(node) in public)
    json.dump({"doc": "", "items": items}, sys.stdout, indent="\t", ensure_ascii=False)
    sys.stdout.write("\n")


def exported(package: Path) -> set[str]:
    tree = ast.parse((package / "__init__.py").read_text(encoding="utf-8"))
    for node in tree.body:
        if isinstance(node, ast.Assign) and any(getattr(t, "id", "") == "__all__" for t in node.targets):
            return {elt.value for elt in node.value.elts}  # type: ignore[attr-defined]
    return set()


def kept_by_store(package: Path) -> set[str]:
    """The classes whose instances Store sets in __init__: self.kv = Kv(link)."""
    tree = ast.parse((package / "store.py").read_text(encoding="utf-8"))
    kept = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Assign) and isinstance(node.value, ast.Call):
            target = node.targets[0]
            if isinstance(target, ast.Attribute) and getattr(target.value, "id", "") == "self":
                kept.add(getattr(node.value.func, "id", ""))
    return kept


def named(node: ast.stmt) -> str:
    if isinstance(node, ast.ClassDef | ast.FunctionDef | ast.AsyncFunctionDef):
        return node.name
    return ""


def item(node: ast.stmt) -> dict:
    if isinstance(node, ast.ClassDef):
        return klass(node)
    return {"name": node.name, "code": signature(node), "doc": markdown(ast.get_docstring(node))}  # type: ignore[attr-defined]


def klass(node: ast.ClassDef) -> dict:
    lines = [head(node)]
    members: list[dict] = []
    body = node.body
    for at, child in enumerate(body):
        if isinstance(child, ast.AnnAssign) and isinstance(child.target, ast.Name) and public(child.target.id):
            lines.append("    " + field(child, body[at + 1] if at + 1 < len(body) else None))
        elif isinstance(child, ast.FunctionDef | ast.AsyncFunctionDef) and public(child.name):
            add_method(members, node.name, child)
    if len(lines) == 1:
        lines.append("    ...")
    return {
        "name": node.name,
        "code": "\n".join(lines),
        "doc": markdown(ast.get_docstring(node)),
        "members": members,
    }


def head(node: ast.ClassDef) -> str:
    """The class line and its decorators, the body left out."""
    shell = copy.copy(node)
    shell.body = [ast.Expr(ast.Constant(...))]
    text = ast.unparse(shell)
    return text[: text.rindex("\n")]


def field(node: ast.AnnAssign, following: ast.stmt | None) -> str:
    """A field and, after it as a comment, the string a dataclass documents it with."""
    line = ast.unparse(node)
    if isinstance(following, ast.Expr) and isinstance(following.value, ast.Constant):
        if isinstance(following.value.value, str):
            line += "  # " + " ".join(following.value.value.split())
    return line


def add_method(members: list[dict], owner: str, node: ast.FunctionDef | ast.AsyncFunctionDef) -> None:
    """A method, its overloads gathered under one name with the docstring one of them has."""
    name = f"{owner}.{node.name}"
    doc = markdown(ast.get_docstring(node))
    if members and members[-1]["name"] == name:
        members[-1]["code"] += "\n" + signature(node)
        members[-1]["doc"] = members[-1]["doc"] or doc
        return
    members.append({"name": name, "code": signature(node), "doc": doc})


def signature(node: ast.FunctionDef | ast.AsyncFunctionDef) -> str:
    """The def as a stub writes it, self left out, wrapped one parameter a line past 88 columns."""
    decorators = [f"@{ast.unparse(d)}" for d in node.decorator_list]
    prefix = "async def" if isinstance(node, ast.AsyncFunctionDef) else "def"
    types = f"[{', '.join(ast.unparse(t) for t in node.type_params)}]" if node.type_params else ""
    returns = f" -> {ast.unparse(node.returns)}" if node.returns else ""
    params = parameters(node.args)
    line = f"{prefix} {node.name}{types}({', '.join(params)}){returns}: ..."
    if len(line) > 88:
        inner = "".join(f"    {p},\n" for p in params)
        line = f"{prefix} {node.name}{types}(\n{inner}){returns}: ..."
    return "\n".join([*decorators, line])


def parameters(args: ast.arguments) -> list[str]:
    positional = [*args.posonlyargs, *args.args]
    defaults = [None] * (len(positional) - len(args.defaults)) + list(args.defaults)
    out = [param(a, d) for a, d in zip(positional, defaults, strict=True)]
    if args.posonlyargs:
        out.insert(len(args.posonlyargs), "/")
    if args.vararg:
        out.append("*" + param(args.vararg, None))
    elif args.kwonlyargs:
        out.append("*")
    out.extend(param(a, d) for a, d in zip(args.kwonlyargs, args.kw_defaults, strict=True))
    if args.kwarg:
        out.append("**" + param(args.kwarg, None))
    if positional and positional[0].arg in ("self", "cls"):
        out.pop(0)
    return out


def param(arg: ast.arg, default: ast.expr | None) -> str:
    text = arg.arg
    if arg.annotation:
        text += f": {ast.unparse(arg.annotation)}"
    if default is not None:
        text += f" = {ast.unparse(default)}" if arg.annotation else f"={ast.unparse(default)}"
    return text


def public(name: str) -> bool:
    return not name.startswith("_")


def markdown(text: str | None) -> str:
    """A docstring as markdown: reST's literal block, after ::, becomes an indented code block."""
    if not text:
        return ""
    return re.sub(r"::\n", ":\n", text).strip()


if __name__ == "__main__":
    main()
