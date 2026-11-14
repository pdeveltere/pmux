#!/usr/bin/env python3
"""Generate src/icons/files.rs from Zed's file icon SVGs and its default icon theme associations.

Usage: python3 tools/zed_icons.py [ZED_SOURCE_DIR]   (default ~/Workspace/oss/zed)

Each SVG becomes a list of vector layers (path, stroke or fill, alpha, fill rule, caps/joins, transform)
that icons::rasterize renders with zeno. Masks and clip paths are dropped (in Zed's icons they're
Figma artifacts clipping to the canvas). Icons are monochrome: any non-none paint is opaque.
"""
import math
import os
import re
import sys
import xml.etree.ElementTree as ET

ZED = os.path.expanduser(sys.argv[1] if len(sys.argv) > 1 else "~/Workspace/oss/zed")
ICONS = os.path.join(ZED, "assets/icons")
THEME = os.path.join(ZED, "crates/theme/src/icon_theme.rs")
OUT = os.path.join(os.path.dirname(__file__), "..", "src", "icons", "files.rs")

IDENT = (1.0, 0.0, 0.0, 1.0, 0.0, 0.0)


def mul(m1, m2):
    """m1 ∘ m2 (m2 applied first). SVG matrix order (a, b, c, d, e, f)."""
    a1, b1, c1, d1, e1, f1 = m1
    a2, b2, c2, d2, e2, f2 = m2
    return (a1 * a2 + c1 * b2, b1 * a2 + d1 * b2, a1 * c2 + c1 * d2, b1 * c2 + d1 * d2, a1 * e2 + c1 * f2 + e1, b1 * e2 + d1 * f2 + f1)


def parse_transform(s):
    m = IDENT
    for name, args in re.findall(r"(\w+)\s*\(([^)]*)\)", s or ""):
        v = [float(x) for x in re.findall(r"[-+]?(?:\d*\.\d+|\d+\.?)(?:[eE][-+]?\d+)?", args)]
        if name == "translate":
            t = (1, 0, 0, 1, v[0], v[1] if len(v) > 1 else 0)
        elif name == "scale":
            t = (v[0], 0, 0, v[1] if len(v) > 1 else v[0], 0, 0)
        elif name == "matrix":
            t = tuple(v)
        elif name == "rotate":
            r = math.radians(v[0])
            t = (math.cos(r), math.sin(r), -math.sin(r), math.cos(r), 0, 0)
            if len(v) == 3:
                t = mul(mul((1, 0, 0, 1, v[1], v[2]), t), (1, 0, 0, 1, -v[1], -v[2]))
        else:
            raise SystemExit(f"unsupported transform {name}")
        m = mul(m, t)
    return m


def num(s, default=0.0):
    if s is None:
        return default
    return float(re.match(r"[-+]?[\d.eE+-]+", s.strip()).group(0))


def fmt(x):
    s = f"{x:.4f}".rstrip("0").rstrip(".")
    return "0" if s in ("-0", "") else s


def rect_path(e):
    x, y, w, h = (num(e.get(k)) for k in ("x", "y", "width", "height"))
    rx, ry = e.get("rx"), e.get("ry")
    rx = num(rx if rx is not None else ry)
    ry = num(ry if ry is not None else e.get("rx"))
    rx, ry = min(rx, w / 2), min(ry, h / 2)
    if rx <= 0 or ry <= 0:
        return f"M{fmt(x)} {fmt(y)}h{fmt(w)}v{fmt(h)}h{fmt(-w)}Z"
    return (
        f"M{fmt(x + rx)} {fmt(y)}h{fmt(w - 2 * rx)}a{fmt(rx)} {fmt(ry)} 0 0 1 {fmt(rx)} {fmt(ry)}"
        f"v{fmt(h - 2 * ry)}a{fmt(rx)} {fmt(ry)} 0 0 1 {fmt(-rx)} {fmt(ry)}h{fmt(-(w - 2 * rx))}"
        f"a{fmt(rx)} {fmt(ry)} 0 0 1 {fmt(-rx)} {fmt(-ry)}v{fmt(-(h - 2 * ry))}a{fmt(rx)} {fmt(ry)} 0 0 1 {fmt(rx)} {fmt(-ry)}Z"
    )


def circle_path(e):
    cx, cy = num(e.get("cx")), num(e.get("cy"))
    rx = num(e.get("r", e.get("rx")))
    ry = num(e.get("r", e.get("ry")))
    return f"M{fmt(cx - rx)} {fmt(cy)}a{fmt(rx)} {fmt(ry)} 0 1 0 {fmt(2 * rx)} 0a{fmt(rx)} {fmt(ry)} 0 1 0 {fmt(-2 * rx)} 0Z"


INHERITED = ("fill", "stroke", "stroke-width", "fill-rule", "fill-opacity", "stroke-opacity", "stroke-linecap", "stroke-linejoin")


def attrs(e):
    a = dict(e.attrib)
    for decl in (a.pop("style", "") or "").split(";"):
        if ":" in decl:
            k, v = decl.split(":", 1)
            a[k.strip()] = v.strip()
    return a


def layers(svg_path):
    root = ET.parse(svg_path).getroot()
    vb = root.get("viewBox")
    view = float(vb.split()[2]) if vb else num(root.get("width"), 16)
    out = []

    def walk(e, inh, m, opacity):
        tag = e.tag.split("}")[-1]
        if tag in ("defs", "mask", "clipPath", "title", "desc", "metadata"):
            return
        a = attrs(e)
        st = dict(inh)
        for k in INHERITED:
            if k in a:
                st[k] = a[k]
        m = mul(m, parse_transform(a.get("transform")))
        opacity *= num(a.get("opacity"), 1.0)
        d = {"path": lambda: e.get("d"), "rect": lambda: rect_path(e), "circle": lambda: circle_path(e), "ellipse": lambda: circle_path(e)}.get(tag)
        if d:
            path = " ".join(d().split())
            if st["fill"] != "none":
                out.append(dict(path=path, stroke=0.0, alpha=opacity * num(st.get("fill-opacity"), 1.0), even_odd=st.get("fill-rule") == "evenodd", cap="butt", join="miter", xf=m))
            if st.get("stroke", "none") != "none":
                out.append(dict(path=path, stroke=num(st.get("stroke-width"), 1.0), alpha=opacity * num(st.get("stroke-opacity"), 1.0), even_odd=False, cap=st.get("stroke-linecap", "butt"), join=st.get("stroke-linejoin", "miter"), xf=m))
        for c in e:
            walk(c, st, m, opacity)

    walk(root, {"fill": "black"}, IDENT, 1.0)
    return view, out


def theme_tables():
    src = open(THEME).read()

    def table(name):
        body = re.search(name + r": &\[\(&str, &\[&str\]\)\] = &\[(.*?)\n\];", src, re.S).group(1)
        return [(k, re.findall(r'"([^"]+)"', v)) for k, v in re.findall(r'\(\s*"([^"]+)",\s*&\[(.*?)\]', body, re.S)]

    icons = dict(re.findall(r'\("([^"]+)", "icons/file_icons/([a-z_]+)\.svg"\)', re.search(r"const FILE_ICONS.*?\n\];", src, re.S).group(0)))
    return table("FILE_STEMS_BY_ICON_KEY"), table("FILE_SUFFIXES_BY_ICON_KEY"), icons


def main():
    stems, suffixes, icons = theme_tables()
    keys = {k for k, _ in stems + suffixes}
    used = sorted({icons.get(k, "file") for k in keys} | {"file"})
    rs = [
        "//! Zed's file icons and its default icon theme's file associations.",
        "//! GENERATED by tools/zed_icons.py from Zed's assets/icons/file_icons — do not edit.",
        "//! Icons: Zed Industries (Apache-2.0) and Lucide (ISC); see the Zed repository's assets/icons/LICENSES.",
        "use super::{Icon, Layer};",
        "",
    ]
    caps = {"butt": 0, "round": 1, "square": 2}
    joins = {"miter": 0, "round": 1, "bevel": 2}
    for name in used:
        view, ls = layers(os.path.join(ICONS, "file_icons", name + ".svg"))
        rs.append(f"pub static {name.upper()}: Icon = Icon {{")
        rs.append(f"    view: {fmt(view)}.0," if "." not in fmt(view) else f"    view: {fmt(view)},")
        rs.append("    layers: &[")
        for l in ls:
            xf = ", ".join(f"{fmt(v)}" + ("" if "." in fmt(v) else ".0") for v in l["xf"])
            f = lambda v: fmt(v) + ("" if "." in fmt(v) else ".0")
            rs.append(
                f'        Layer {{ path: "{l["path"]}", stroke: {f(l["stroke"])}, alpha: {f(l["alpha"])}, even_odd: {str(l["even_odd"]).lower()}, '
                f'cap: {caps[l["cap"]]}, join: {joins[l["join"]]}, xf: [{xf}] }},'
            )
        rs.append("    ],")
        rs.append("};")
        rs.append("")

    def arms(table):
        lines = []
        for key, names in table:
            icon = icons.get(key, "file").upper()
            pats = " | ".join(f'"{n}"' for n in names)
            lines.append(f"        {pats} => &{icon},")
        return lines

    rs += [
        "/// Icon for a file name, as Zed picks it: the stem (`Dockerfile`), else the longest dotted suffix",
        "/// (`docker-compose.yml`, `d.ts` → `ts`, `.gitignore` → `gitignore`), else the generic file.",
        "pub fn for_name(name: &str) -> &'static Icon {",
        "    let stem = std::path::Path::new(name).file_stem().and_then(|s| s.to_str()).unwrap_or(name);",
        "    if let Some(i) = by_stem(stem) {",
        "        return i;",
        "    }",
        "    let mut s = name;",
        "    loop {",
        "        if let Some(i) = by_suffix(s) {",
        "            return i;",
        "        }",
        "        match s.split_once('.') {",
        "            Some((_, rest)) => s = rest,",
        "            None => return &FILE,",
        "        }",
        "    }",
        "}",
        "",
        "#[allow(unreachable_patterns)]",
        "fn by_stem(s: &str) -> Option<&'static Icon> {",
        "    Some(match s {",
        *arms(stems),
        "        _ => return None,",
        "    })",
        "}",
        "",
        "#[allow(unreachable_patterns)]",
        "fn by_suffix(s: &str) -> Option<&'static Icon> {",
        "    Some(match s {",
        *arms(suffixes),
        "        _ => return None,",
        "    })",
        "}",
        "",
        "#[cfg(test)]",
        "pub static ALL: &[&Icon] = &[" + ", ".join("&" + n.upper() for n in used) + "];",
        "",
    ]
    open(OUT, "w").write("\n".join(rs))
    print(f"wrote {OUT}: {len(used)} icons")


main()
