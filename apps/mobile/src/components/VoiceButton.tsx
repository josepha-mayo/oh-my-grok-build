import { useEffect, useRef, useState } from "react";
import { Mic, MicOff } from "lucide-react";

interface VoiceButtonProps {
  onTranscript: (text: string) => void;
  disabled?: boolean;
}

export function VoiceButton({ onTranscript, disabled }: VoiceButtonProps) {
  const [supported, setSupported] = useState(false);
  const [listening, setListening] = useState(false);
  const recognitionRef = useRef<any>(null);
  const stoppedByUser = useRef(false);

  useEffect(() => {
    if (typeof window === "undefined") return;
    const w = window as unknown as { SpeechRecognition?: unknown; webkitSpeechRecognition?: unknown };
    setSupported(!!w.SpeechRecognition || !!w.webkitSpeechRecognition);
  }, []);

  useEffect(() => {
    return () => {
      recognitionRef.current?.abort?.();
      recognitionRef.current = null;
    };
  }, []);

  if (!supported) return null;

  const stopListening = () => {
    stoppedByUser.current = true;
    try {
      recognitionRef.current?.stop();
    } catch {
      // Ignore errors from stopping an inactive recognition.
    }
  };

  const startListening = () => {
    const w = window as unknown as {
      SpeechRecognition?: new () => unknown;
      webkitSpeechRecognition?: new () => unknown;
    };
    const RecognitionCtor = w.SpeechRecognition || w.webkitSpeechRecognition;
    if (!RecognitionCtor) return;

    const recognition = new (RecognitionCtor as new () => any)();
    recognition.continuous = false;
    recognition.interimResults = false;
    recognition.lang = navigator.language || "en-US";

    recognitionRef.current = recognition;
    stoppedByUser.current = false;

    recognition.onresult = (event: any) => {
      const parts: string[] = [];
      for (let i = event.resultIndex; i < event.results.length; i++) {
        if (event.results[i].isFinal) {
          parts.push(event.results[i][0].transcript as string);
        }
      }
      if (parts.length > 0) {
        onTranscript(parts.join(""));
      }
    };

    recognition.onerror = () => {
      recognitionRef.current = null;
      setListening(false);
    };

    recognition.onend = () => {
      recognitionRef.current = null;
      setListening(false);
    };

    try {
      recognition.start();
      setListening(true);
    } catch {
      recognitionRef.current = null;
      setListening(false);
    }
  };

  const toggle = () => {
    if (listening) {
      stopListening();
    } else {
      startListening();
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
