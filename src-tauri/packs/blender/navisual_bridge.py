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
BUTTON_UNITS = 1.7
SEP_UNITS = 0.6
TOP_PAD_UNITS = 0.45

_HANDLERS = {"layout": _q_layout, "state": _q_state, "tools": _q_tools}


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
