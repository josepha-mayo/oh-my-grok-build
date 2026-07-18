#!/usr/bin/env python3
"""Minimal MCP server for desktop computer use using pyautogui."""

import base64
import io
import json
import os
import sys
from datetime import datetime, timezone

def check_deps():
    try:
        import pyautogui
        return pyautogui
    except Exception as exc:  # pragma: no cover
        raise RuntimeError(
            "pyautogui is not installed. Install it with: pip install pyautogui"
        ) from exc


def check_desktop_control_allowed():
    if os.environ.get("OMGB_ALLOW_DESKTOP_CONTROL") != "1":
        raise RuntimeError(
            "Desktop control is disabled by default. Set OMGB_ALLOW_DESKTOP_CONTROL=1 to enable."
        )

def send(msg: dict):
    print(json.dumps(msg), flush=True)

MAX_IMAGE_DIMENSION = 4096


def screenshot(pyautogui, width: int | None = None, height: int | None = None):
    img = pyautogui.screenshot()
    if width is not None and height is not None:
        if width <= 0 or height <= 0:
            raise ValueError("width and height must be positive integers")
        screen_w, screen_h = pyautogui.size()
        width = min(width, screen_w, MAX_IMAGE_DIMENSION)
        height = min(height, screen_h, MAX_IMAGE_DIMENSION)
        img = img.resize((width, height))
    buf = io.BytesIO()
    img.save(buf, format="PNG")
    return base64.b64encode(buf.getvalue()).decode("utf-8")

def handle(msg: dict, pyautogui):
    method = msg.get("method")
    id_ = msg.get("id")
    if method == "initialize":
        try:
            check_desktop_control_allowed()
            send({"jsonrpc": "2.0", "id": id_, "result": {"protocolVersion": "2024-11-05", "capabilities": {}, "serverInfo": {"name": "omgb-computer", "version": "0.1.0"}}})
        except Exception as exc:
            send({"jsonrpc": "2.0", "id": id_, "error": {"code": -32000, "message": str(exc)}})
        return
    if method == "notifications/initialized":
        return
    if not pyautogui:
        send({"jsonrpc": "2.0", "id": id_, "error": {"code": -32002, "message": "Server not initialized"}})
        return
    if method == "tools/list":
        try:
            check_desktop_control_allowed()
        except Exception as exc:
            send({"jsonrpc": "2.0", "id": id_, "error": {"code": -32000, "message": str(exc)}})
            return
        tools = [
            {"name": "computer_screenshot", "description": "Take a screenshot of the desktop.", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "computer_get_size", "description": "Return the screen width and height.", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "computer_click", "description": "Click at screen coordinates.", "inputSchema": {"type": "object", "properties": {"x": {"type": "number"}, "y": {"type": "number"}, "button": {"type": "string"}}, "required": ["x", "y"]}},
            {"name": "computer_type", "description": "Type text.", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}},
            {"name": "computer_key", "description": "Press a key or hotkey combination.", "inputSchema": {"type": "object", "properties": {"key": {"type": "string"}, "modifiers": {"type": "array", "items": {"type": "string"}}}, "required": ["key"]}},
            {"name": "computer_scroll", "description": "Scroll the mouse wheel.", "inputSchema": {"type": "object", "properties": {"clicks": {"type": "number"}, "x": {"type": "number"}, "y": {"type": "number"}}, "required": ["clicks"]}},
            {"name": "computer_move", "description": "Move the mouse to coordinates.", "inputSchema": {"type": "object", "properties": {"x": {"type": "number"}, "y": {"type": "number"}}, "required": ["x", "y"]}},
            {"name": "computer_double_click", "description": "Double-click at screen coordinates.", "inputSchema": {"type": "object", "properties": {"x": {"type": "number"}, "y": {"type": "number"}, "button": {"type": "string"}}, "required": ["x", "y"]}},
            {"name": "computer_drag", "description": "Drag the mouse from one point to another.", "inputSchema": {"type": "object", "properties": {"x1": {"type": "number"}, "y1": {"type": "number"}, "x2": {"type": "number"}, "y2": {"type": "number"}, "button": {"type": "string"}}, "required": ["x1", "y1", "x2", "y2"]}},
            {"name": "computer_get_mouse_position", "description": "Return the current mouse cursor coordinates.", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "computer_screenshot_region", "description": "Take a screenshot of a region of the screen.", "inputSchema": {"type": "object", "properties": {"x": {"type": "number"}, "y": {"type": "number"}, "width": {"type": "number"}, "height": {"type": "number"}}, "required": ["x", "y", "width", "height"]}},
            {"name": "computer_wait", "description": "Pause for the given number of seconds.", "inputSchema": {"type": "object", "properties": {"seconds": {"type": "number"}}, "required": ["seconds"]}},
        ]
        send({"jsonrpc": "2.0", "id": id_, "result": {"tools": tools}})
        return
    if method == "tools/call":
        try:
            check_desktop_control_allowed()
        except Exception as exc:
            send({"jsonrpc": "2.0", "id": id_, "result": {"content": [{"type": "text", "text": str(exc)}], "isError": True}})
            return
        params = msg.get("params", {})
        name = params.get("name", "")
        args = params.get("arguments", {})
        try:
            result = call_tool(name, args, pyautogui)
            if isinstance(result, dict) and result.get("type") == "image":
                send({"jsonrpc": "2.0", "id": id_, "result": {"content": [result], "isError": False}})
            else:
                send({"jsonrpc": "2.0", "id": id_, "result": {"content": [{"type": "text", "text": str(result)}], "isError": False}})
        except Exception as exc:
            send({"jsonrpc": "2.0", "id": id_, "result": {"content": [{"type": "text", "text": str(exc)}], "isError": True}})
        return
    send({"jsonrpc": "2.0", "id": id_, "error": {"code": -32601, "message": f"Method not found: {method}"}})

MAX_WAIT_SECONDS = 300

VALID_BUTTONS = {"left", "middle", "right"}


def _screen_size(pyautogui):
    width, height = pyautogui.size()
    return width, height


def _clamp(value, min_value, max_value):
    return max(min_value, min(value, max_value))


def _valid_button(button):
    if button not in VALID_BUTTONS:
        raise ValueError(f"button must be one of {VALID_BUTTONS}, got {button}")


def call_tool(name: str, args: dict, pyautogui):
    if name == "computer_screenshot":
        data = screenshot(pyautogui)
        return {"type": "image", "data": data, "mimeType": "image/png"}
    if name == "computer_get_size":
        width, height = pyautogui.size()
        return f"Screen size: {width}x{height}"
    if name == "computer_click":
        x = int(args.get("x", 0))
        y = int(args.get("y", 0))
        button = str(args.get("button", "left"))
        _valid_button(button)
        screen_w, screen_h = _screen_size(pyautogui)
        x = _clamp(x, 0, screen_w)
        y = _clamp(y, 0, screen_h)
        pyautogui.click(x, y, button=button)
        return f"Clicked ({x}, {y}) with {button} button."
    if name == "computer_type":
        text = str(args.get("text", ""))
        pyautogui.typewrite(text, interval=0.01)
        return f"Typed: {text}"
    if name == "computer_key":
        key = str(args.get("key", ""))
        if not key:
            raise ValueError("key is required")
        modifiers = args.get("modifiers", [])
        if not isinstance(modifiers, list):
            modifiers = [modifiers]
        modifiers = [str(m) for m in modifiers if m]
        try:
            for m in modifiers:
                pyautogui.keyDown(m)
            pyautogui.keyDown(key)
            pyautogui.keyUp(key)
            for m in reversed(modifiers):
                pyautogui.keyUp(m)
        except Exception:
            for m in modifiers:
                pyautogui.keyUp(m)
            pyautogui.keyUp(key)
            raise
        return f"Pressed: {'+'.join(modifiers + [key])}"
    if name == "computer_scroll":
        clicks = int(args.get("clicks", 0))
        x = args.get("x")
        y = args.get("y")
        screen_w, screen_h = _screen_size(pyautogui)
        if x is not None and y is not None:
            x = _clamp(int(x), 0, screen_w)
            y = _clamp(int(y), 0, screen_h)
            pyautogui.scroll(clicks, x, y)
        elif x is not None or y is not None:
            raise ValueError("provide both x and y or neither")
        else:
            pyautogui.scroll(clicks)
        return f"Scrolled {clicks} clicks."
    if name == "computer_move":
        x = int(args.get("x", 0))
        y = int(args.get("y", 0))
        screen_w, screen_h = _screen_size(pyautogui)
        x = _clamp(x, 0, screen_w)
        y = _clamp(y, 0, screen_h)
        pyautogui.moveTo(x, y)
        return f"Moved to ({x}, {y})."
    if name == "computer_double_click":
        x = int(args.get("x", 0))
        y = int(args.get("y", 0))
        button = str(args.get("button", "left"))
        _valid_button(button)
        screen_w, screen_h = _screen_size(pyautogui)
        x = _clamp(x, 0, screen_w)
        y = _clamp(y, 0, screen_h)
        pyautogui.doubleClick(x, y, button=button)
        return f"Double-clicked ({x}, {y}) with {button} button."
    if name == "computer_drag":
        x1 = int(args.get("x1", 0))
        y1 = int(args.get("y1", 0))
        x2 = int(args.get("x2", 0))
        y2 = int(args.get("y2", 0))
        button = str(args.get("button", "left"))
        _valid_button(button)
        screen_w, screen_h = _screen_size(pyautogui)
        x1 = _clamp(x1, 0, screen_w)
        y1 = _clamp(y1, 0, screen_h)
        x2 = _clamp(x2, 0, screen_w)
        y2 = _clamp(y2, 0, screen_h)
        pyautogui.moveTo(x1, y1)
        pyautogui.dragTo(x2, y2, button=button)
        return f"Dragged from ({x1}, {y1}) to ({x2}, {y2}) with {button} button."
    if name == "computer_get_mouse_position":
        x, y = pyautogui.position()
        return f"Mouse position: ({x}, {y})."
    if name == "computer_screenshot_region":
        x = int(args.get("x", 0))
        y = int(args.get("y", 0))
        width = int(args.get("width", 0))
        height = int(args.get("height", 0))
        if width <= 0 or height <= 0:
            raise ValueError("width and height must be positive integers")
        screen_w, screen_h = _screen_size(pyautogui)
        x = _clamp(x, 0, screen_w)
        y = _clamp(y, 0, screen_h)
        width = _clamp(width, 1, screen_w - x)
        height = _clamp(height, 1, screen_h - y)
        img = pyautogui.screenshot(region=(x, y, width, height))
        buf = io.BytesIO()
        img.save(buf, format="PNG")
        data = base64.b64encode(buf.getvalue()).decode("utf-8")
        return {"type": "image", "data": data, "mimeType": "image/png"}
    if name == "computer_wait":
        seconds = float(args.get("seconds", 0))
        if seconds < 0:
            raise ValueError("seconds must be non-negative")
        if seconds > MAX_WAIT_SECONDS:
            raise ValueError(f"seconds must be at most {MAX_WAIT_SECONDS}")
        pyautogui.sleep(seconds)
        return f"Waited {seconds}s."
    raise RuntimeError(f"Unknown tool: {name}")

def main():
    pyautogui = None
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        if msg.get("method") == "initialize":
            try:
                pyautogui = check_deps()
            except Exception as exc:
                send({"jsonrpc": "2.0", "id": msg.get("id"), "error": {"code": -32000, "message": str(exc)}})
                continue
        handle(msg, pyautogui)

if __name__ == "__main__":
    main()
