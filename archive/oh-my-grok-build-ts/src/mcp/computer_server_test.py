import base64
import io
import json
import os
import unittest
from PIL import Image

import computer_server


class FakePyautogui:
    def __init__(self):
        self.calls = []
        self._size = (1920, 1080)
        self._position = (100, 200)
        self.KEYBOARD_KEYS = ["ctrl", "alt", "shift", "enter", "a"]

    def size(self):
        return self._size

    def position(self):
        return self._position

    def screenshot(self, region=None):
        if region:
            return Image.new("RGB", (region[2], region[3]), color="red")
        return Image.new("RGB", self._size, color="blue")

    def click(self, x, y, button="left"):
        self.calls.append(("click", x, y, button))

    def typewrite(self, text, interval=None):
        self.calls.append(("typewrite", text, interval))

    def keyDown(self, key):
        self.calls.append(("keyDown", key))

    def keyUp(self, key):
        self.calls.append(("keyUp", key))

    def scroll(self, clicks, x=None, y=None):
        self.calls.append(("scroll", clicks, x, y))

    def moveTo(self, x, y):
        self.calls.append(("moveTo", x, y))

    def doubleClick(self, x, y, button="left"):
        self.calls.append(("doubleClick", x, y, button))

    def dragTo(self, x, y, button="left"):
        self.calls.append(("dragTo", x, y, button))

    def sleep(self, seconds):
        self.calls.append(("sleep", seconds))


class TestComputerServer(unittest.TestCase):
    def setUp(self):
        self.pyautogui = FakePyautogui()
        os.environ["OMGB_ALLOW_DESKTOP_CONTROL"] = "1"

    def tearDown(self):
        os.environ.pop("OMGB_ALLOW_DESKTOP_CONTROL", None)

    def test_screenshot_returns_base64_png(self):
        data = computer_server.screenshot(self.pyautogui)
        raw = base64.b64decode(data)
        img = Image.open(io.BytesIO(raw))
        self.assertEqual(img.format, "PNG")

    def test_screenshot_respects_dimensions(self):
        data = computer_server.screenshot(self.pyautogui, 100, 100)
        raw = base64.b64decode(data)
        img = Image.open(io.BytesIO(raw))
        self.assertEqual(img.size, (100, 100))

    def test_screenshot_rejects_non_positive_dimensions(self):
        with self.assertRaises(ValueError):
            computer_server.screenshot(self.pyautogui, 0, 100)

    def test_call_get_size(self):
        result = computer_server.call_tool("computer_get_size", {}, self.pyautogui)
        self.assertIn("1920x1080", result)

    def test_call_click_clamps_and_records(self):
        result = computer_server.call_tool(
            "computer_click", {"x": -10, "y": 5000, "button": "left"}, self.pyautogui
        )
        self.assertIn("(0, 1080)", result)
        self.assertEqual(self.pyautogui.calls[0], ("click", 0, 1080, "left"))

    def test_call_type(self):
        result = computer_server.call_tool("computer_type", {"text": "hello"}, self.pyautogui)
        self.assertIn("Typed: hello", result)
        self.assertEqual(self.pyautogui.calls[0][0], "typewrite")

    def test_call_key_with_modifiers(self):
        result = computer_server.call_tool(
            "computer_key", {"key": "enter", "modifiers": ["ctrl", "shift"]}, self.pyautogui
        )
        self.assertIn("Pressed: ctrl+shift+enter", result)

    def test_call_key_rejects_invalid_key(self):
        with self.assertRaises(ValueError):
            computer_server.call_tool("computer_key", {"key": "notakey"}, self.pyautogui)

    def test_call_scroll(self):
        result = computer_server.call_tool(
            "computer_scroll", {"clicks": 3, "x": 50, "y": 50}, self.pyautogui
        )
        self.assertIn("Scrolled 3 clicks", result)

    def test_call_move(self):
        result = computer_server.call_tool("computer_move", {"x": 50, "y": 50}, self.pyautogui)
        self.assertIn("Moved to (50, 50)", result)

    def test_call_double_click(self):
        result = computer_server.call_tool(
            "computer_double_click", {"x": 10, "y": 20}, self.pyautogui
        )
        self.assertIn("Double-clicked", result)

    def test_call_drag(self):
        result = computer_server.call_tool(
            "computer_drag", {"x1": 0, "y1": 0, "x2": 100, "y2": 100}, self.pyautogui
        )
        self.assertIn("Dragged from (0, 0) to (100, 100)", result)

    def test_call_screenshot_region(self):
        result = computer_server.call_tool(
            "computer_screenshot_region",
            {"x": 0, "y": 0, "width": 50, "height": 50},
            self.pyautogui,
        )
        self.assertEqual(result["type"], "image")
        self.assertTrue(result["data"])

    def test_call_wait(self):
        result = computer_server.call_tool("computer_wait", {"seconds": 0.1}, self.pyautogui)
        self.assertIn("Waited 0.1s", result)

    def test_handle_initialize(self):
        messages = []

        def capture(msg):
            messages.append(msg)

        original_send = computer_server.send
        computer_server.send = capture
        try:
            computer_server.handle({"method": "initialize", "id": 1, "params": {}}, self.pyautogui)
        finally:
            computer_server.send = original_send

        self.assertEqual(len(messages), 1)
        self.assertIn("result", messages[0])

    def test_handle_initialize_rejects_without_env(self):
        os.environ.pop("OMGB_ALLOW_DESKTOP_CONTROL", None)
        messages = []

        def capture(msg):
            messages.append(msg)

        original_send = computer_server.send
        computer_server.send = capture
        try:
            computer_server.handle({"method": "initialize", "id": 1, "params": {}}, self.pyautogui)
        finally:
            computer_server.send = original_send

        self.assertIn("error", messages[0])

    def test_handle_tools_call(self):
        messages = []

        def capture(msg):
            messages.append(msg)

        original_send = computer_server.send
        computer_server.send = capture
        try:
            computer_server.handle(
                {
                    "method": "tools/call",
                    "id": 2,
                    "params": {
                        "name": "computer_get_size",
                        "arguments": {},
                    },
                },
                self.pyautogui,
            )
        finally:
            computer_server.send = original_send

        self.assertEqual(messages[0]["result"]["isError"], False)


if __name__ == "__main__":
    unittest.main()
