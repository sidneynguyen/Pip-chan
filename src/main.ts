import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { invoke } from "@tauri-apps/api/core";
import "./style.css";

type PipEvent = {
  source: "codex" | "claude" | "test" | string;
  event: "ready" | "attention" | string;
};

const pip = document.querySelector<HTMLElement>("#pip")!;
const image = document.querySelector<HTMLImageElement>("#pip-image")!;
const bubble = document.querySelector<HTMLElement>("#bubble")!;
const copy = document.querySelector<HTMLElement>("#bubble-copy")!;
const dismiss = document.querySelector<HTMLButtonElement>("#dismiss")!;
const hint = document.querySelector<HTMLElement>("#hint")!;

let timeout: number | undefined;
let readyCount = 0;

function showEvent(event: PipEvent) {
  window.clearTimeout(timeout);
  const isAttention = event.event === "attention";

  if (isAttention) {
    copy.textContent = `${event.source === "claude" ? "Claude" : "Codex"} needs your approval.`;
  } else {
    readyCount += 1;
    copy.textContent = readyCount === 1
      ? "Baka! I’m waiting."
      : `Baka! ${readyCount} agents are waiting.`;
  }

  pip.classList.toggle("is-alert", true);
  bubble.hidden = false;
  bubble.classList.remove("arriving");
  void bubble.offsetWidth;
  bubble.classList.add("arriving");

  timeout = window.setTimeout(hideBubble, isAttention ? 12_000 : 8_000);
}

function hideBubble() {
  bubble.hidden = true;
  pip.classList.remove("is-alert");
  readyCount = 0;
}

dismiss.addEventListener("pointerdown", (event) => {
  event.stopPropagation();
  hideBubble();
});

pip.addEventListener("pointerdown", async (event) => {
  if ((event.target as HTMLElement).closest("button")) return;
  hint.classList.add("is-hidden");
  await getCurrentWindow().startDragging();
});

void listen<PipEvent>("pip:event", ({ payload }) => showEvent(payload));
void invoke<PipEvent | null>("initial_event").then((event) => event && showEvent(event));
