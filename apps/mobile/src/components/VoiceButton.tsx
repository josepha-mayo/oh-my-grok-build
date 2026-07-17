import { useEffect, useState } from "react";
import { Mic, MicOff } from "lucide-react";

interface VoiceButtonProps {
  onTranscript: (text: string) => void;
  disabled?: boolean;
}

export function VoiceButton({ onTranscript, disabled }: VoiceButtonProps) {
  const [supported, setSupported] = useState(false);
  const [listening, setListening] = useState(false);

  useEffect(() => {
    if (typeof window === "undefined") return;
    const w = window as unknown as { SpeechRecognition?: unknown; webkitSpeechRecognition?: unknown };
    setSupported(!!w.SpeechRecognition || !!w.webkitSpeechRecognition);
  }, []);

  if (!supported) return null;

  const toggle = () => {
    if (listening) {
      setListening(false);
      return;
    }

    const w = window as unknown as {
      SpeechRecognition?: new () => unknown;
      webkitSpeechRecognition?: new () => unknown;
    };
    const RecognitionCtor = w.SpeechRecognition || w.webkitSpeechRecognition;
    if (!RecognitionCtor) return;

    const recognition = new (RecognitionCtor as new () => any)();
    recognition.continuous = false;
    recognition.interimResults = true;
    recognition.lang = navigator.language || "en-US";

    let finalTranscript = "";

    recognition.onresult = (event: any) => {
      let interim = "";
      for (let i = event.resultIndex; i < event.results.length; i++) {
        const transcript = event.results[i][0].transcript as string;
        if (event.results[i].isFinal) {
          finalTranscript += transcript;
        } else {
          interim += transcript;
        }
      }
      onTranscript(finalTranscript + interim);
    };

    recognition.onerror = () => {
      setListening(false);
    };

    recognition.onend = () => {
      setListening(false);
      if (finalTranscript) onTranscript(finalTranscript);
    };

    try {
      recognition.start();
      setListening(true);
    } catch {
      setListening(false);
    }
  };

  return (
    <button
      type="button"
      className={`icon-button voice-button ${listening ? "listening" : ""}`}
      onClick={toggle}
      disabled={disabled}
      title={listening ? "Stop listening" : "Voice input"}
    >
      {listening ? <MicOff size={20} /> : <Mic size={20} />}
    </button>
  );
}
