#!/usr/bin/env python3
"""Generates crates/aun-api/src/generated.rs from the api-videos OpenAPI document.

    python3 tools/codegen/openapi_to_rust.py spec/openapi/api-videos.json crates/aun-api/src/generated.rs

Standard library only, deterministic output (CI diffs it against the committed file).

What it emits, per operation (operationId `get_VideoScheduleList` → `get_video_schedule_list`):
  * `<Op>Params`   path, query and header parameters (optional ones are `Option`)
  * `<Op>Body`     JSON request body (when the schema is an object)
  * `<Op>Response` the 200/201 JSON response, nested objects as their own structs
  * an `ApiClient::<op>()` async method and an entry in `OPERATIONS`

Response structs are tolerant: every struct derives `Default` and uses `#[serde(default)]`, so a
missing required field decodes to its default instead of failing the whole response. Unknown or
ambiguous schemas (no type, anyOf/oneOf, multipart) map to `serde_json::Value`.

The current spec (before Epic 9.3) has doubled path prefixes, e.g. `/api/videos/api/videos/upload`;
`normalise_path` collapses them to the routes the Worker actually serves.
"""

from __future__ import annotations

import json
import re
import sys

RUST_KEYWORDS = {
    "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn", "for",
    "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
    "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where",
    "while", "async", "await", "dyn", "abstract", "become", "box", "do", "final", "macro",
    "override", "priv", "typeof", "unsized", "virtual", "yield", "try", "gen",
}

DOUBLED = re.compile(r"^(/api(?:/[^/{}]+)*?)\1(/.*)?$")


def normalise_path(path: str) -> str:
    m = DOUBLED.match(path)
    if m:
        return m.group(1) + (m.group(2) or "")
    return path


def snake(name: str) -> str:
    name = re.sub(r"[^0-9A-Za-z]+", "_", name)
    name = re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", name)
    name = re.sub(r"([A-Z]+)([A-Z][a-z])", r"\1_\2", name)
    name = re.sub(r"_+", "_", name).strip("_").lower()
    if not name:
        name = "field"
    if name[0].isdigit():
        name = "n" + name
    return name


def pascal(name: str) -> str:
    return "".join(p[:1].upper() + p[1:] for p in snake(name).split("_") if p)


def field_ident(name: str) -> str:
    ident = snake(name)
    if ident in RUST_KEYWORDS:
        ident = f"r#{ident}"
    return ident


def doc_lines(text: str | None, indent: str) -> list[str]:
    if not text:
        return []
    text = text.replace("\r", "")
    out = []
    for line in text.strip().split("\n"):
        line = line.rstrip()
        out.append(f"{indent}///{(' ' + line) if line else ''}")
    return out


def schema_types(schema: dict) -> tuple[set[str], bool]:
    t = schema.get("type")
    if t is None:
        return set(), False
    if isinstance(t, list):
        types = {x for x in t if x != "null"}
        return types, "null" in t
    return {t}, False


class Gen:
    def __init__(self) -> None:
        self.structs: list[str] = []
        self.names: set[str] = set()

    def unique(self, name: str) -> str:
        base, n = name, 2
        while name in self.names:
            name = f"{base}{n}"
            n += 1
        self.names.add(name)
        return name

    def rust_type(self, schema: dict | None, name_hint: str) -> str:
        """Rust type for a schema (without the Option wrapper for nullability)."""
        if not isinstance(schema, dict) or "anyOf" in schema or "oneOf" in schema or "allOf" in schema:
            return "serde_json::Value"
        types, _ = schema_types(schema)
        if len(types) != 1:
            return "serde_json::Value"
        (t,) = types
        if t == "string":
            return "String"
        if t == "integer":
            return "i64"
        if t == "number":
            return "f64"
        if t == "boolean":
            return "bool"
        if t == "array":
            return f"Vec<{self.rust_type(schema.get('items'), name_hint + 'Item')}>"
        if t == "object":
            props = schema.get("properties")
            if props:
                return self.struct(name_hint, schema)
            extra = schema.get("additionalProperties")
            if isinstance(extra, dict):
                return f"std::collections::BTreeMap<String, {self.rust_type(extra, name_hint + 'Value')}>"
            return "serde_json::Map<String, serde_json::Value>"
        return "serde_json::Value"

    def struct(self, name_hint: str, schema: dict, derive_default: bool = True) -> str:
        name = self.unique(name_hint)
        required = set(schema.get("required", []))
        lines = doc_lines(schema.get("description"), "")
        lines.append("#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]")
        lines.append("#[serde(default)]")
        lines.append(f"pub struct {name} {{")
        used: set[str] = set()
        for prop, sub in schema["properties"].items():
            ident = field_ident(prop)
            while ident in used:
                ident += "_"
            used.add(ident)
            sub = sub if isinstance(sub, dict) else {}
            ty = self.rust_type(sub, name + pascal(prop))
            _, nullable = schema_types(sub)
            optional = prop not in required or nullable
            if optional and ty != "serde_json::Value":
                ty = f"Option<{ty}>"
            lines.extend(doc_lines(sub.get("description"), "    "))
            attrs = []
            if ident.removeprefix("r#") != prop:
                attrs.append(f'rename = "{prop}"')
            if optional:
                attrs.append('skip_serializing_if = "Option::is_none"' if ty.startswith("Option<") else
                             'skip_serializing_if = "serde_json::Value::is_null"')
            if attrs:
                lines.append(f"    #[serde({', '.join(attrs)})]")
            lines.append(f"    pub {ident}: {ty},")
        lines.append("}")
        self.structs.append("\n".join(lines))
        return name


def generate(spec: dict) -> str:
    g = Gen()
    ops = []
    for raw_path, methods in spec["paths"].items():
        path = normalise_path(raw_path)
        if not path.startswith("/api/"):
            continue  # monitoring, internal and test routes are not part of the client
        for method, op in methods.items():
            if method not in ("get", "post", "put", "patch", "delete") or not isinstance(op, dict):
                continue
            ops.append((path, method, op))
    ops.sort(key=lambda x: snake(x[2].get("operationId") or f"{x[1]}_{x[0]}"))

    methods_out = []
    builders_out = []
    table = []
    seen_ops: set[str] = set()
    for path, method, op in ops:
        op_id = op.get("operationId") or f"{method}_{path}"
        fn = snake(op_id)
        if fn in seen_ops:
            continue
        seen_ops.add(fn)
        base = pascal(op_id)
        auth_required = "Authentication Required" in (op.get("description") or "")

        # Parameters (dedupe by location+name; the spec repeats a few)
        params = []
        seen_params = set()
        for p in op.get("parameters", []):
            key = (p["in"], p["name"])
            if key in seen_params or p["in"] not in ("path", "query", "header"):
                continue
            seen_params.add(key)
            params.append(p)
        params_ty = None
        if params:
            name = g.unique(f"{base}Params")
            lines = [f"/// Parameters for [`ApiClient::{fn}`].",
                     "#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]",
                     f"pub struct {name} {{"]
            used = set()
            for p in params:
                ident = field_ident(p["name"])
                if p["in"] == "header":
                    ident = field_ident("header_" + p["name"])
                while ident in used:
                    ident += "_"
                used.add(ident)
                p["_ident"] = ident
                schema = p.get("schema", {})
                ty = g.rust_type(schema, name + pascal(p["name"]))
                if ty.startswith(("Vec<", "std::", "serde_json::Map")) or ty not in ("String", "i64", "f64", "bool"):
                    ty = "String"
                _, nullable = schema_types(schema)
                optional = p["in"] != "path" and (not p.get("required", False) or nullable)
                p["_optional"] = optional
                lines.extend(doc_lines(p.get("description") or schema.get("description"), "    "))
                lines.append(f"    pub {ident}: {'Option<' + ty + '>' if optional else ty},")
            lines.append("}")
            g.structs.append("\n".join(lines))
            params_ty = name

        # Request body
        body_ty = None
        body_kind = None
        content = (op.get("requestBody") or {}).get("content", {})
        if "application/json" in content:
            schema = content["application/json"].get("schema", {})
            ty = g.rust_type(schema, f"{base}Body")
            body_ty, body_kind = ty, "json"
        elif "multipart/form-data" in content:
            body_kind = "multipart"

        # Response
        resp_ty = "serde_json::Value"
        resp_kind = "json"
        for code in ("200", "201", "202"):
            r = op.get("responses", {}).get(code)
            if not r:
                continue
            rc = r.get("content", {})
            if "application/json" in rc:
                resp_ty = g.rust_type(rc["application/json"].get("schema"), f"{base}Response")
            elif rc:
                resp_ty, resp_kind = "String", "text"
            # A 2xx without declared content still usually returns JSON: keep it as a Value.
            break
        else:
            if "204" in op.get("responses", {}):
                resp_ty, resp_kind = "()", "empty"

        # Request builder (requests::<op>) + client method
        doc = doc_lines(op.get("summary"), "    ")
        doc.append(f"    ///\n    /// `{method.upper()} {path}`{' (authenticated)' if auth_required else ''}")
        bargs, cargs = [], []
        if params_ty:
            bargs.append(f"params: &{params_ty}")
            cargs.append("params")
        if body_kind == "json":
            bargs.append(f"body: &{body_ty}")
            cargs.append("body")
        elif body_kind == "multipart":
            bargs.append("body: crate::client::Multipart")
            cargs.append("body")
        body = []
        path_params = [p for p in params if p["in"] == "path"]
        if path_params:
            fmt = path
            for p in path_params:
                fmt = fmt.replace("{" + p["name"] + "}", "{}")
            fmt_args = ", ".join(f"crate::client::encode_path(&params.{p['_ident']}.to_string())" for p in path_params)
            body.append(f"        let path = format!(\"{fmt}\", {fmt_args});")
        else:
            body.append(f"        let path = \"{path}\".to_owned();")
        body.append(f"        let mut req = RequestSpec::new(reqwest::Method::{method.upper()}, path, \"{op_id}\");")
        if auth_required:
            body.append("        req.auth = crate::client::AuthMode::Required;")
        for p in params:
            if p["in"] == "path":
                continue
            target = "query" if p["in"] == "query" else "headers"
            ident = p["_ident"]
            if p["_optional"]:
                body.append(f"        if let Some(v) = &params.{ident} {{ req.{target}.push((\"{p['name']}\".into(), v.to_string())); }}")
            else:
                body.append(f"        req.{target}.push((\"{p['name']}\".into(), params.{ident}.to_string()));")
        if body_kind == "json":
            body.append("        req.body = crate::client::Body::Json(serde_json::to_value(body)?);")
        elif body_kind == "multipart":
            body.append("        req.body = crate::client::Body::Multipart(body);")
        body.append("        Ok(req)")
        builders_out.append("\n".join(doc + [f"    pub fn {fn}({', '.join(bargs)}) -> crate::Result<RequestSpec> {{"] + body + ["    }"]))
        exec_fn = {"json": "execute_json", "text": "execute_text", "empty": "execute_empty"}[resp_kind]
        methods_out.append("\n".join(doc + [
            f"    pub async fn {fn}({', '.join(['&self'] + bargs)}) -> crate::Result<{resp_ty}> {{",
            f"        self.{exec_fn}(requests::{fn}({', '.join(cargs)})?).await",
            "    }",
        ]))
        table.append(
            f"    OperationInfo {{ id: \"{op_id}\", method: \"{method.upper()}\", path: \"{path}\", auth_required: {str(auth_required).lower()} }},"
        )

    title = spec.get("info", {}).get("title", "")
    version = spec.get("info", {}).get("version", "")
    out = [
        "// @generated by tools/codegen/openapi_to_rust.py from spec/openapi/api-videos.json. Do not edit.",
        f"// Source: {title} {version}",
        "#![allow(clippy::all, clippy::pedantic, missing_docs, rustdoc::all, unused_mut)]",
        "",
        "use crate::client::ApiClient;",
        "",
        "/// Static metadata for an operation (used by the offline queue and tests).",
        "#[derive(Debug, Clone, Copy, PartialEq, Eq)]",
        "pub struct OperationInfo {",
        "    pub id: &'static str,",
        "    pub method: &'static str,",
        "    pub path: &'static str,",
        "    pub auth_required: bool,",
        "}",
        "",
        f"/// Every operation in the spec ({len(table)}).",
        "pub const OPERATIONS: &[OperationInfo] = &[",
        *table,
        "];",
        "",
        *[s + "\n" for s in g.structs],
        "/// Request builders: each returns the request an operation would send, without sending it",
        "/// (for the offline queue, or to set an idempotency key).",
        "pub mod requests {",
        "    use super::*;",
        "    use crate::client::RequestSpec;",
        "",
        "\n\n".join(builders_out),
        "}",
        "",
        "impl ApiClient {",
        "\n\n".join(methods_out),
        "}",
        "",
    ]
    return "\n".join(out)


def main() -> None:
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    with open(sys.argv[1], encoding="utf-8") as f:
        spec = json.load(f)
    with open(sys.argv[2], "w", encoding="utf-8") as f:
        f.write(generate(spec))


if __name__ == "__main__":
    main()
