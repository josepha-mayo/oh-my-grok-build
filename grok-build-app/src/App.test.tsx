import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import App from "./App.js";

const mockClient = vi.hoisted(() => ({
  ready: vi.fn().mockResolvedValue(undefined),
  initialize: vi
    .fn()
    .mockResolvedValue({ authMethods: [{ id: "xai.api_key" }] }),
  authenticate: vi.fn().mockResolvedValue(undefined),
  newSession: vi.fn().mockResolvedValue({ sessionId: "session-abc" }),
  setMode: vi.fn().mockResolvedValue(true),
  setModel: vi.fn().mockResolvedValue(true),
  setEffort: vi.fn().mockResolvedValue(true),
  prompt: vi.fn().mockResolvedValue(undefined),
  runTerminalCmd: vi.fn().mockResolvedValue({ output: "ok", exitCode: 0 }),
  close: vi.fn(),
}));

vi.mock("./acp.js", () => ({
  AcpClient: vi.fn(() => mockClient),
}));

vi.mock("./QrScanner.js", () => ({
  QrScanner: () => <div data-testid="qr-scanner" />,
}));

const SERVER_URL = "ws://localhost:7331/ws?server-key=ABC123";

async function connect() {
  await userEvent.type(screen.getByLabelText(/server url/i), SERVER_URL);
  await userEvent.click(screen.getByRole("button", { name: /connect/i }));
  await waitFor(() => expect(mockClient.ready).toHaveBeenCalled());
}

describe("App", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.stubGlobal("prompt", vi.fn().mockReturnValue("ok"));
    window.localStorage.clear();
    window.sessionStorage.clear();
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("renders the connect panel", () => {
    render(<App />);
    expect(screen.getByText("Grok Build Mobile")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /connect/i }),
    ).toBeInTheDocument();
  });

  it("connects to a server and shows the chat composer", async () => {
    render(<App />);
    await connect();

    expect(mockClient.initialize).toHaveBeenCalled();
    expect(mockClient.authenticate).toHaveBeenCalledWith("xai.api_key");
    expect(mockClient.newSession).toHaveBeenCalled();
    expect(mockClient.setMode).toHaveBeenCalledWith("session-abc", "ask");

    expect(
      await screen.findByPlaceholderText(/ask grok build/i),
    ).toBeInTheDocument();
  });

  it("sends a prompt when the user submits the composer", async () => {
    render(<App />);
    await connect();

    const composer = await screen.findByPlaceholderText(/ask grok build/i);
    await userEvent.type(composer, "hello grok");
    await userEvent.click(screen.getByRole("button", { name: /send/i }));

    await waitFor(() =>
      expect(mockClient.prompt).toHaveBeenCalledWith("session-abc", [
        { type: "text", text: "hello grok" },
      ]),
    );
  });

  it("runs /loop via runTerminalCmd", async () => {
    render(<App />);
    await connect();

    const composer = await screen.findByPlaceholderText(/ask grok build/i);
    await userEvent.type(composer, "/loop 2 fix the bug");
    await userEvent.click(screen.getByRole("button", { name: /send/i }));

    await waitFor(() =>
      expect(mockClient.runTerminalCmd).toHaveBeenCalledWith([
        "loop",
        "--max-iterations",
        "2",
        "fix the bug",
      ]),
    );
  });

  it("runs /schedule list via runTerminalCmd", async () => {
    render(<App />);
    await connect();

    const composer = await screen.findByPlaceholderText(/ask grok build/i);
    await userEvent.type(composer, "/schedule list");
    await userEvent.click(screen.getByRole("button", { name: /send/i }));

    await waitFor(() =>
      expect(mockClient.runTerminalCmd).toHaveBeenCalledWith([
        "schedule",
        "list",
      ]),
    );
  });

  it("/yolo toggles and updates the session mode", async () => {
    render(<App />);
    await connect();

    const composer = await screen.findByPlaceholderText(/ask grok build/i);
    await userEvent.type(composer, "/yolo");
    await userEvent.click(screen.getByRole("button", { name: /send/i }));

    await waitFor(() =>
      expect(mockClient.setMode).toHaveBeenCalledWith("session-abc", "code"),
    );
  });

  it("/model and /effort call setModel and setEffort", async () => {
    render(<App />);
    await connect();

    const composer = await screen.findByPlaceholderText(/ask grok build/i);
    await userEvent.type(composer, "/model grok-3{enter}");
    await waitFor(() =>
      expect(mockClient.setModel).toHaveBeenCalledWith("session-abc", "grok-3"),
    );

    await userEvent.clear(composer);
    await userEvent.type(composer, "/effort high{enter}");
    await waitFor(() =>
      expect(mockClient.setEffort).toHaveBeenCalledWith("session-abc", "high"),
    );
  });

  it("/btw sends a side-note prompt", async () => {
    render(<App />);
    await connect();

    const composer = await screen.findByPlaceholderText(/ask grok build/i);
    await userEvent.type(composer, "/btw remember to check docs");
    await userEvent.click(screen.getByRole("button", { name: /send/i }));

    await waitFor(() =>
      expect(mockClient.prompt).toHaveBeenCalledWith(
        "session-abc",
        expect.arrayContaining([
          expect.objectContaining({
            type: "text",
            text: expect.stringContaining("remember to check docs"),
          }),
        ]),
      ),
    );
  });

  it("/clear removes messages", async () => {
    render(<App />);
    await connect();

    const composer = await screen.findByPlaceholderText(/ask grok build/i);
    await userEvent.type(composer, "hello");
    await userEvent.click(screen.getByRole("button", { name: /send/i }));

    await screen.findByText("hello");

    await userEvent.type(composer, "/clear{enter}");
    await waitFor(() =>
      expect(screen.queryByText("hello")).not.toBeInTheDocument(),
    );
  });
});
