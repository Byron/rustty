#!/usr/bin/env python3
"""Generate option schemas/defaults without linking or running C tmux."""
from __future__ import annotations

import argparse
import ast
from pathlib import Path
import re

from generate_command_metadata import STRING, strings

from source_paths import TMUXX, c_source
OUTPUT = TMUXX / "crates/tmuxx-core/src/options_metadata.rs"


def number(expression, macros, active=()):
    def evaluate(node):
        if isinstance(node, ast.Constant) and isinstance(node.value, int):
            return node.value
        if isinstance(node, ast.Name) and node.id in macros and node.id not in active:
            return number(macros[node.id], macros, (*active, node.id))
        if isinstance(node, ast.UnaryOp) and isinstance(node.op, (ast.USub, ast.UAdd)):
            return (-1 if isinstance(node.op, ast.USub) else 1) * evaluate(node.operand)
        if isinstance(node, ast.BinOp):
            a, b = evaluate(node.left), evaluate(node.right)
            operations = {ast.Add: lambda: a + b, ast.Sub: lambda: a - b,
                          ast.Mult: lambda: a * b, ast.BitOr: lambda: a | b,
                          ast.LShift: lambda: a << b}
            if type(node.op) in operations:
                return operations[type(node.op)]()
        raise ValueError(f"unsupported numeric expression {expression!r}")
    return evaluate(ast.parse(expression.strip(), mode="eval").body)


def read_entries(root=None):
    root = c_source() if root is None else root
    source = (root / "options-table.c").read_text(encoding="utf-8")
    header = (root / "tmux.h").read_text(encoding="utf-8")
    macros = {"INT_MAX": "2147483647", "UINT_MAX": "4294967295",
              "SHRT_MAX": "32767", "USHRT_MAX": "65535"}
    for data in (header, (root / "compat.h").read_text(encoding="utf-8"), source):
        for name, value in re.findall(r"^#define\s+(\w+)[ \t]+([^\n]*)", data.replace("\\\n", ""), re.M):
            macros[name] = re.sub(r"/\*.*?\*/", "", value).strip()
    for body in re.findall(r"enum (?:box_lines|pane_lines) \{(.*?)\};", header, re.S):
        value = -1
        for item in body.split(","):
            item = item.strip()
            if not item:
                continue
            name, _, expression = item.partition("=")
            value = number(expression, macros) if expression else value + 1
            macros[name.strip()] = str(value)
    arrays = {}
    for name, body in re.findall(r"static const char \*(\w+)\[\] = \{(.*?)\};", source, re.S):
        # Commas within quoted format strings are data, not array separators.
        tokens = re.findall(STRING + r"|\w+|,", body)
        values, part = [], []
        for token in [*tokens, ","]:
            if token == ",":
                expression = " ".join(part)
                if expression and expression != "NULL":
                    values.append(strings(expression, macros))
                part = []
            else:
                part.append(token)
        arrays[name] = values
    table = source.split("const struct options_table_entry options_table[] = {", 1)[1]
    entries = []
    for match in re.finditer(r'\{ \.name = ("[^"\\]+"),(.*?)\n\t\}', table, re.S):
        name = strings(match[1], macros)
        body = re.sub(r"/\*.*?\*/", "", match[2], flags=re.S).rstrip().rstrip(",") + ",\n.__end = 0"
        fields = dict(re.findall(r"\.(\w+)\s*=\s*(.*?),(?=\s*\.\w+\s*=)", body, re.S))
        kind = fields["type"].removeprefix("OPTIONS_TABLE_")
        scope = fields["scope"]
        scope = "s" if "SERVER" in scope else "c" if "SESSION" in scope else "w"
        flags = fields.get("flags", "")
        choices = arrays.get(fields.get("choices"), [])
        default = strings(fields["default_str"], macros) if "default_str" in fields else ""
        if "default_num" in fields:
            expression = fields["default_num"].strip()
            if kind == "KEY":
                keys = {r"'\177'": "C-?", "'b'|KEYC_CTRL": "C-b", "KEYC_NONE": "None"}
                default = keys[expression]
            else:
                value = number(expression, macros)
                default = choices[value] if kind == "CHOICE" else ("on" if value else "off") if kind == "FLAG" else "default" if kind == "COLOUR" and value == 8 else str(value)
        feature = ""
        for directive, block in re.findall(r"#ifdef (\w+)\n(.*?)#endif", table, re.S):
            if match[0] in block:
                feature = directive
        entries.append(dict(position=match.start(), name=name, kind=kind, scope=scope, pane="PANE" in fields["scope"],
                            array="IS_ARRAY" in flags, pattern=strings(fields.get("pattern", '""'), macros), style="IS_STYLE" in flags, colour="IS_COLOUR" in flags, hook=False,
                            choices=choices, default=default,
                            defaults=arrays.get(fields.get("default_arr"), []),
                            separator=strings(fields.get("separator", '" ,"'), macros),
                            minimum=number(fields.get("minimum", "0"), macros),
                            maximum=number(fields.get("maximum", "0"), macros), feature=feature))
    for match in re.finditer(r'OPTIONS_TABLE_(?:(PANE|WINDOW)_)?HOOK\(\s*(' + STRING + r'),\s*(' + STRING + r')', table):
        entries.append(dict(position=match.start(), name=strings(match[2], macros), kind="COMMAND", scope="w" if match[1] else "c",
                            pane=match[1] == "PANE", array=True, pattern="", style=False, colour=False, hook=True, choices=[],
                            default=strings(match[3], macros), defaults=[], separator="", minimum=0, maximum=0, feature=""))
    for match in re.finditer(r"OPTIONS_TABLE_AFTER_HOOK\((" + STRING + r")\)", table):
        entries.append(dict(position=match.start(), name="after-" + strings(match[1], macros), kind="COMMAND", scope="c", pane=False,
                            array=True, pattern="", style=False, colour=False, hook=True, choices=[], default="", defaults=[],
                            separator="", minimum=0, maximum=0, feature=""))
    if len({entry["name"] for entry in entries}) != len(entries):
        raise ValueError("duplicate option metadata")
    return sorted(entries, key=lambda entry: entry["position"])


def generate(root=None):
    root = c_source() if root is None else root
    def quoted(value):
        escapes = {'"': '\\"', '\\': '\\\\', '\n': '\\n', '\r': '\\r', '\t': '\\t'}
        return '"' + ''.join(escapes.get(char, "\\u{" + format(ord(char), "x") + "}" if ord(char) < 32 or ord(char) == 127 else char) for char in value) + '"'
    lines = ["// Generated by rust/conformance/generate_option_metadata.py; do not edit.",
             "// Platform/build defaults are tagged; supported writes remain an explicit allowlist.",
             "#[rustfmt::skip]", "const SOURCE_METADATA: &[Metadata] = &["]
    for entry in read_entries(root):
        kind = entry["kind"]
        if entry["array"]:
            kind = "Kind::Array(&[" + ", ".join("'" + quoted(c)[1:-1].replace("'", "\\'") + "'" for c in entry["separator"]) + "])"
        elif kind == "CHOICE":
            kind = "Kind::Choice(&[" + ", ".join(map(quoted, entry["choices"])) + "])"
        elif kind == "NUMBER":
            kind = f'Kind::Number({entry["minimum"]}, {entry["maximum"]})'
        else:
            kind = {"FLAG": "Kind::Flag", "KEY": "Kind::Key", "COLOUR": "Kind::String", "STRING": "Kind::String", "COMMAND": "Kind::String"}[kind]
        lines.append("    Metadata { spec: Spec { " + f'name: {quoted(entry["name"])}, scope: b\'{entry["scope"]}\', kind: {kind}, default: {quoted(entry["default"])}, pane: {str(entry["pane"]).lower()}' + " }, " +
                     f'pattern: {quoted(entry["pattern"])}, hook: {str(entry["hook"]).lower()}, style: {str(entry["style"]).lower()}, colour: {str(entry["colour"]).lower()}, value_type: {quoted(entry["kind"])}, feature: {quoted(entry["feature"])}, defaults: &[' + ", ".join(map(quoted, entry["defaults"])) + "] },")
    return "\n".join([*lines, "];", ""])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    result = generate()
    if args.check:
        if not OUTPUT.exists() or OUTPUT.read_text(encoding="utf-8") != result:
            parser.exit(1, "C option metadata changed; regenerate and review schemas, defaults and parity coverage.\n")
    else:
        OUTPUT.write_text(result)


if __name__ == "__main__":
    main()
