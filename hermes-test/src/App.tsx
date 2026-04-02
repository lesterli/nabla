import { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { Message } from "./types";
import ChatWindow from "./components/ChatWindow";
import MessageInput from "./components/MessageInput";

function updateLastAssistant(prev: Message[], content: string): Message[] {
  const updated = [...prev];
  const last = updated[updated.length - 1];
  if (last && last.role === "assistant") {
    updated[updated.length - 1] = { ...last, content };
  }
  return updated;
}

export default function App() {
  const [messages, setMessages] = useState<Message[]>([]);
  const [isStreaming, setIsStreaming] = useState(false);
  const [connected, setConnected] = useState<boolean | null>(null);
  const streamBuffer = useRef("");
  const messagesRef = useRef(messages);
  messagesRef.current = messages;

  useEffect(() => {
    invoke<boolean>("health_check")
      .then(() => setConnected(true))
      .catch(() => setConnected(false));
  }, []);

  useEffect(() => {
    const unlistenStream = listen<{ content: string }>("hermes://stream", (e) => {
      streamBuffer.current += e.payload.content;
      setMessages((prev) => updateLastAssistant(prev, streamBuffer.current));
    });

    const unlistenDone = listen("hermes://done", () => {
      setIsStreaming(false);
      streamBuffer.current = "";
    });

    return () => {
      unlistenStream.then((fn) => fn());
      unlistenDone.then((fn) => fn());
    };
  }, []);

  const handleSend = useCallback(
    async (text: string) => {
      const trimmed = text.trim();
      if (isStreaming || !trimmed) return;

      const userMsg: Message = { role: "user", content: trimmed };
      const assistantMsg: Message = { role: "assistant", content: "" };

      setMessages((prev) => [...prev, userMsg, assistantMsg]);
      setIsStreaming(true);
      streamBuffer.current = "";

      try {
        await invoke<string>("send_message", {
          message: trimmed,
          history: messagesRef.current,
        });
      } catch (err) {
        setMessages((prev) => updateLastAssistant(prev, `Error: ${err}`));
        setIsStreaming(false);
      }
    },
    [isStreaming],
  );

  const statusClass = connected === true ? "connected" : connected === false ? "disconnected" : "";
  const statusText = connected === true ? "Connected" : connected === false ? "Disconnected" : "Checking...";

  return (
    <div className="app">
      <div className="header">
        <h1>Hermes</h1>
        <span className={`status ${statusClass}`}>{statusText}</span>
      </div>
      {messages.length === 0 ? (
        <div className="empty-state">Send a message to start</div>
      ) : (
        <ChatWindow messages={messages} isStreaming={isStreaming} />
      )}
      <MessageInput onSend={handleSend} disabled={isStreaming} />
    </div>
  );
}
