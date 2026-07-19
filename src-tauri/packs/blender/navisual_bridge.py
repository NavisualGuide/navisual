# Navisual Bridge — read-only layout/state queries for the Navisual screen guide.
#
# Ships with the Navisual Blender nav-pack ("ship the recipe" — script-adapters-plan.md
# §3.5). Install once: Edit → Preferences → Add-ons → Install… → pick this file → enable
# "Navisual Bridge". The checkbox IS the consent: nothing runs until you enable it.
#
# What it does (and all it does): opens a localhost-only socket (127.0.0.1:47611) that
# answers three READ-ONLY queries about the Blender UI — region layout, active tool/mode,
# and the tool-shelf item rects — so Navisual can point at tools with exact geometry
# instead of image matching. It never calls operators, never modifies data, and never
# accepts connections from other machines.

bl_info = {
    "name": "Navisual Bridge",
    "author": "Navisual",
    "version": (1, 0, 0),
    "blender": (3, 0, 0),
    "location": "background (localhost:47611)",
    "description": "Read-only UI layout/state bridge for the Navisual screen guide",
    "category": "System",
}

import bpy
import json
import queue
import socket
import threading

PORT = 47611
# Requests cross from the socket thread to Blender's main thread here — bpy data must
# only be touched on the main thread (the bpy.app.timers pattern).
_requests: "queue.Queue" = queue.Queue()
_server_socket = None
_stop = threading.Event()


# ---------------------------------------------------------------- main-thread handlers

def _window():
    wm = bpy.context.window_manager
    return wm.windows[0] if wm.windows else None


def _q_layout(_req):
    win = _window()
    if win is None:
        return {"error": "no window"}
    out = {"window": [win.width, win.height], "areas": []}
    for area in win.screen.areas:
        a = {
            "type": area.type,
            "rect": [area.x, area.y, area.width, area.height],
            "regions": [],
        }
        for region in area.regions:
            if region.width <= 1 or region.height <= 1:
                continue  # collapsed
            a["regions"].append(
                {"type": region.type, "rect": [region.x, region.y, region.width, region.height]}
            )
        out["areas"].append(a)
    return out


def _q_state(_req):
    ctx = bpy.context
    tool = None
    try:
        from bl_ui.space_toolsystem_common import ToolSelectPanelHelper
        t = ToolSelectPanelHelper.tool_active_from_context(ctx)
        tool = t.idname if t else None
    except Exception:
        pass
    return {
        "blender": bpy.app.version_string,
        "workspace": ctx.workspace.name if ctx.workspace else None,
        "mode": getattr(ctx, "mode", None),
        "active_tool": tool,
        "ui_scale": ctx.preferences.system.ui_scale,
    }


def _tool_members(item):
    """Idnames + labels for one shelf slot (a single tool or a flyout group)."""
    # ToolDef is itself a tuple subclass — detect groups as tuples whose members
    # each carry an idname, not by isinstance alone (the 3.6 spike lesson).
    if hasattr(item, "idname"):
        return [{"idname": item.idname, "label": getattr(item, "label", "") or ""}]
    if isinstance(item, tuple):
        out = []
        for t in item:
            if hasattr(t, "idname"):
                out.append({"idname": t.idname, "label": getattr(t, "label", "") or ""})
        return out
    return []


def _q_tools(_req):
    """Ordered tool-shelf slots with DERIVED window-relative rects (bottom-up Y, like
    all bpy region coords). Derivation: the shelf is a single column of fixed-height
    buttons stacked from the region's top; separators consume a fixed fraction.
    Constants calibrated live against Blender 5.1.2 at ui_scale 1.0 (2026-07-19)."""
    ctx = bpy.context
    win = _window()
    if win is None:
        return {"error": "no window"}
    from bl_ui.space_toolsystem_common import ToolSelectPanelHelper

    area = next((a for a in win.screen.areas if a.type == "VIEW_3D"), None)
    if area is None:
        return {"error": "no VIEW_3D area"}
    region = next((r for r in area.regions if r.type == "TOOLS"), None)
    if region is None or region.width <= 1:
        return {"error": "tool shelf hidden"}

    cls = ToolSelectPanelHelper._tool_class_from_space_type("VIEW_3D")
    mode = ctx.mode
    try:
        items = list(cls.tools_from_context(ctx, mode=mode))
    except Exception:
        items = list(cls.tools_from_context(ctx))

    scale = ctx.preferences.system.ui_scale
    # Calibrated: each shelf button is ~1.6 widget-units tall; a separator ~0.45.
    unit = 20.0 * scale
    button_h = BUTTON_UNITS * unit
    sep_h = SEP_UNITS * unit
    top_pad = TOP_PAD_UNITS * unit

    # View2D scroll: view-space coord at the region's TOP; content starts at view y=0.
    try:
        scroll_top = region.view2d.region_to_view(0, region.height - 1)[1]
    except Exception:
        scroll_top = 0.0

    slots = []
    # y grows DOWN in content space; convert to region bottom-up at the end.
    cursor = top_pad
    for item in items:
        if item is None:
            cursor += sep_h
            continue
        members = _tool_members(item)
        if not members:
            continue
        top = cursor
        bottom = cursor + button_h
        cursor = bottom
        # content-space → region-relative bottom-up: region_y = region.height − (content_y − scroll_offset)
        # scroll_top is the content-space y visible at the region top (0 when unscrolled).
        y_top_regionspace = region.height - (top - scroll_top)
        rect_bottom_up = [
            region.x,
            region.y + int(y_top_regionspace - button_h),
            region.width,
            int(button_h),
        ]
        slots.append({"members": members, "rect": rect_bottom_up, "group": len(members) > 1})
    return {
        "window": [win.width, win.height],
        "region": [region.x, region.y, region.width, region.height],
        "ui_scale": scale,
        "slots": slots,
    }


# Calibration constants (units of the 20px widget unit). Tuned live on 5.1.2.
BUTTON_UNITS = 1.65
SEP_UNITS = 0.6
TOP_PAD_UNITS = 0.45

# Properties nav-bar tab stack (calibrated 2026-07-19 on 5.1.2 @ ui_scale 1.0: pitch
# 28px, +8px before a new section, first tab top ~8.5px; band-fit max error ~2px).
TAB_PITCH_UNITS = 1.4
TAB_SECTION_EXTRA_UNITS = 0.4
TAB_TOP_PAD_UNITS = 0.425
TAB_H_UNITS = 1.3

# Visible tabs for a MESH active object, in nav-bar order, with Blender's canonical
# tooltip names (what the AI most often echoes as target_text). Verified visually
# against 5.1.2 factory (14 tabs incl. Collection; sections break before RENDER,
# COLLECTION, OBJECT). Other object types change the OBJECT-section subset — they
# return an error until verified, and the caller falls through to templates.
TABS_MESH = [
    ("TOOL", "Tool"),
    ("RENDER", "Render Properties"),
    ("OUTPUT", "Output Properties"),
    ("VIEW_LAYER", "View Layer Properties"),
    ("SCENE", "Scene Properties"),
    ("WORLD", "World Properties"),
    ("COLLECTION", "Collection Properties"),
    ("OBJECT", "Object Properties"),
    ("MODIFIER", "Modifier Properties"),
    ("PARTICLES", "Particle Properties"),
    ("PHYSICS", "Physics Properties"),
    ("CONSTRAINT", "Object Constraint Properties"),
    ("DATA", "Object Data Properties"),
    ("MATERIAL", "Material Properties"),
]
TAB_SECTION_BREAK_BEFORE = {1, 6, 7}


def _q_tabs(_req):
    """Properties nav-bar tabs with DERIVED window-relative rects (bottom-up Y)."""
    ctx = bpy.context
    win = _window()
    if win is None:
        return {"error": "no window"}
    obj = ctx.active_object
    if obj is None or obj.type != "MESH":
        return {
            "error": f"tab layout only verified for MESH active object (have {obj.type if obj else 'none'})"
        }
    area = next((a for a in win.screen.areas if a.type == "PROPERTIES"), None)
    if area is None:
        return {"error": "no PROPERTIES area"}
    region = next((r for r in area.regions if r.type == "NAVIGATION_BAR"), None)
    if region is None or region.width <= 1:
        return {"error": "nav bar hidden"}
    space = next((s for s in area.spaces if s.type == "PROPERTIES"), None)
    active = space.context if space else None

    scale = ctx.preferences.system.ui_scale
    unit = 20.0 * scale
    pitch = TAB_PITCH_UNITS * unit
    extra = TAB_SECTION_EXTRA_UNITS * unit
    top_pad = TAB_TOP_PAD_UNITS * unit
    tab_h = TAB_H_UNITS * unit
    try:
        scroll_top = region.view2d.region_to_view(0, region.height - 1)[1]
    except Exception:
        scroll_top = 0.0

    tabs = []
    sections = 0
    for i, (ident, name) in enumerate(TABS_MESH):
        if i in TAB_SECTION_BREAK_BEFORE:
            sections += 1
        top = top_pad + i * pitch + sections * extra
        y_top_regionspace = region.height - (top - scroll_top)
        tabs.append(
            {
                "id": ident,
                "name": name,
                "active": ident == active,
                "rect": [
                    region.x,
                    region.y + int(y_top_regionspace - tab_h),
                    region.width,
                    int(tab_h),
                ],
            }
        )
    return {
        "window": [win.width, win.height],
        "region": [region.x, region.y, region.width, region.height],
        "ui_scale": scale,
        "active": active,
        "tabs": tabs,
    }


# VIEW_3D header right-side toggle cluster — RIGHT-ANCHORED fixed items (calibrated
# 2026-07-19 on 5.1.2 @ ui_scale 2.0, maximized area; offsets in widget units from the
# header region's right edge, scale-multiplicative like everything else).
# (right_offset_u = distance from right edge to the item's RIGHT side, width_u).
# (stem, display name, KEY tokens, right_offset_u of the LEFT edge, width_u).
# Key tokens are the DISTINGUISHING words: the adapter matches an item when the AI's
# target contains one of its keys, rather than requiring a phrasing to appear in a
# hardcoded alias list. Live 2026-07-19: "Wireframe Shading" matched no alias and fell
# to templates — the header cluster is the one surface whose names we GUESS (there is no
# enumerable widget list, unlike the tool shelf), so matching must be key-based.
# An item with no keys is the generic fallback (the shading popover).
HEADER_ITEMS = [
    ("shading_dropdown", "Viewport Shading", [], 1.45, 0.9),
    ("rendered", "Rendered", ["rendered", "render"], 2.35, 0.9),
    ("material_preview", "Material Preview", ["material"], 3.3, 0.9),
    ("solid", "Solid", ["solid"], 4.25, 0.9),
    ("wireframe", "Wireframe", ["wireframe"], 5.2, 0.9),
    ("xray", "X-Ray", ["xray", "ray"], 6.45, 0.9),
    ("overlays", "Show Overlays", ["overlay"], 8.7, 0.9),
]
# Words that describe the shading widget generally — a target made only of these
# (plus filler) means the popover, not a specific mode.
HEADER_GENERIC_TOKENS = ["viewport", "shading", "shade", "mode", "view", "the", "button", "icon"]


def _q_header(_req):
    """VIEW_3D header toggles with DERIVED window-relative rects (bottom-up Y)."""
    ctx = bpy.context
    win = _window()
    if win is None:
        return {"error": "no window"}
    area = next((a for a in win.screen.areas if a.type == "VIEW_3D"), None)
    if area is None:
        return {"error": "no VIEW_3D area"}
    region = next((r for r in area.regions if r.type == "HEADER"), None)
    if region is None or region.width <= 1:
        return {"error": "header hidden"}
    unit = 20.0 * ctx.preferences.system.ui_scale
    right = region.x + region.width
    items = []
    for stem, name, keys, right_off_u, w_u in HEADER_ITEMS:
        w = int(w_u * unit)
        x0 = int(right - right_off_u * unit)
        items.append(
            {
                "stem": stem,
                "name": name,
                "keys": keys,
                # Full header height vertically — the click target is the button row.
                "rect": [x0, region.y, w, region.height],
            }
        )
    return {
        "window": [win.width, win.height],
        "region": [region.x, region.y, region.width, region.height],
        "ui_scale": ctx.preferences.system.ui_scale,
        "generic_tokens": HEADER_GENERIC_TOKENS,
        "items": items,
    }


_HANDLERS = {
    "layout": _q_layout,
    "state": _q_state,
    "tools": _q_tools,
    "tabs": _q_tabs,
    "header": _q_header,
}


def _process_queue():
    while not _requests.empty():
        holder = _requests.get()
        try:
            q = holder["req"].get("q", "")
            handler = _HANDLERS.get(q)
            holder["resp"] = handler(holder["req"]) if handler else {"error": f"unknown query {q!r}"}
        except Exception as e:  # noqa: BLE001 — a query must never kill the timer
            holder["resp"] = {"error": repr(e)}
        holder["evt"].set()
    return 0.05 if not _stop.is_set() else None


def _serve():
    global _server_socket
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("127.0.0.1", PORT))
    srv.listen(4)
    srv.settimeout(1.0)
    _server_socket = srv
    while not _stop.is_set():
        try:
            conn, _addr = srv.accept()
        except socket.timeout:
            continue
        except OSError:
            break
        try:
            conn.settimeout(2.0)
            line = conn.makefile("r", encoding="utf-8").readline()
            req = json.loads(line) if line.strip() else {}
            holder = {"req": req, "resp": None, "evt": threading.Event()}
            _requests.put(holder)
            if holder["evt"].wait(timeout=2.0):
                payload = holder["resp"]
            else:
                payload = {"error": "main-thread timeout"}
            conn.sendall((json.dumps(payload) + "\n").encode("utf-8"))
        except Exception:
            pass
        finally:
            try:
                conn.close()
            except OSError:
                pass


_thread = None


def register():
    global _thread
    _stop.clear()
    _thread = threading.Thread(target=_serve, name="navisual-bridge", daemon=True)
    _thread.start()
    bpy.app.timers.register(_process_queue, persistent=True)


def unregister():
    _stop.set()
    global _server_socket
    if _server_socket is not None:
        try:
            _server_socket.close()
        except OSError:
            pass
        _server_socket = None


if __name__ == "__main__":
    register()
