import { useEffect, useRef } from "react";
import type { Message } from "../types";

interface Props {
  messages: Message[];
  isStreaming: boolean;
}

export default function ChatWindow({ messages, isStreaming }: Props) {
  const endRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    endRef.current?.scrollIntoView({ behavior: isStreaming ? "instant" : "smooth" });
  }, [messages, isStreaming]);

  return (
    <div className="chat-window">
      {messages.map((msg, i) => {
        const isLast = i === messages.length - 1;
        const isStreamingMsg = isLast && msg.role === "assistant" && isStreaming;
        return (
          <div
            key={i}
            className={`message ${msg.role}${isStreamingMsg ? " streaming" : ""}`}
          >
            {msg.content || (isStreamingMsg ? "" : "\u00A0")}
          </div>
        );
      })}
      <div ref={endRef} />
    </div>
  );
}
