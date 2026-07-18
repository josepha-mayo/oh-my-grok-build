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

def screenshot(pyautogui, width: int | None = None, height: int | None = None):
    img = pyautogui.screenshot()
    if width and height:
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
        tools = [
            {"name": "computer_screenshot", "description": "Take a screenshot of the desktop.", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "computer_get_size", "description": "Return the screen width and height.", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "computer_click", "description": "Click at screen coordinates.", "inputSchema": {"type": "object", "properties": {"x": {"type": "number"}, "y": {"type": "number"}, "button": {"type": "string"}}, "required": ["x", "y"]}},
            {"name": "computer_type", "description": "Type text.", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}},
            {"name": "computer_key", "description": "Press a key or hotkey combination.", "inputSchema": {"type": "object", "properties": {"key": {"type": "string"}, "modifiers": {"type": "array", "items": {"type": "string"}}}, "required": ["key"]}},
            {"name": "computer_scroll", "description": "Scroll the mouse wheel.", "inputSchema": {"type": "object", "properties": {"clicks": {"type": "number"}, "x": {"type": "number"}, "y": {"type": "number"}}, "required": ["clicks"]}},
            {"name": "computer_move", "description": "Move the mouse to coordinates.", "inputSchema": {"type": "object", "properties": {"x": {"type": "number"}, "y": {"type": "number"}}, "required": ["x", "y"]}},
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
        button = args.get("button", "left")
        pyautogui.click(x, y, button=button)
        return f"Clicked ({x}, {y}) with {button} button."
    if name == "computer_type":
        text = args.get("text", "")
        pyautogui.typewrite(text, interval=0.01)
        return f"Typed: {text}"
    if name == "computer_key":
        key = args.get("key", "")
        modifiers = args.get("modifiers", [])
        if modifiers:
            pyautogui.keyDown(*modifiers)
            pyautogui.keyDown(key)
            pyautogui.keyUp(key)
            pyautogui.keyUp(*modifiers)
        else:
            pyautogui.press(key)
        return f"Pressed: {'+'.join(modifiers + [key])}"
    if name == "computer_scroll":
        clicks = int(args.get("clicks", 0))
        x = args.get("x")
        y = args.get("y")
        if x is not None and y is not None:
            pyautogui.scroll(clicks, int(x), int(y))
        else:
            pyautogui.scroll(clicks)
        return f"Scrolled {clicks} clicks."
    if name == "computer_move":
        x = int(args.get("x", 0))
        y = int(args.get("y", 0))
        pyautogui.moveTo(x, y)
        return f"Moved to ({x}, {y})."
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
