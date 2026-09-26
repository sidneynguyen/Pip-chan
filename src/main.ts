import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { invoke } from "@tauri-apps/api/core";
import "./style.css";

type PipEvent = {
  source: "codex" | "claude" | "test" | string;
  event: "idle" | "thinking" | "ready" | "attention" | string;
};

type PipStatus = "idle" | "thinking" | "ready";

const pip = document.querySelector<HTMLElement>("#pip")!;
const image = document.querySelector<HTMLImageElement>("#pip-image")!;
const bubble = document.querySelector<HTMLElement>("#bubble")!;
const copy = document.querySelector<HTMLElement>("#bubble-copy")!;
const dismiss = document.querySelector<HTMLButtonElement>("#dismiss")!;
const hint = document.querySelector<HTMLElement>("#hint")!;

const imageByStatus: Record<PipStatus, string> = {
  idle: new URL("./assets/pip-idle.png", import.meta.url).href,
  thinking: new URL("./assets/pip-thinking.png", import.meta.url).href,
  ready: new URL("./assets/pip-ready.png", import.meta.url).href,
};

const altByStatus: Record<PipStatus, string> = {
  idle: "Pip-chan is idle",
  thinking: "Pip-chan is thinking",
  ready: "Pip-chan is waiting",
};

let timeout: number | undefined;

function showEvent(event: PipEvent) {
  window.clearTimeout(timeout);

  if (event.event === "idle") {
    hideBubble();
    return;
  }

  if (event.event === "thinking") {
    bubble.hidden = true;
    setStatus("thinking");
    return;
  }

  const isAttention = event.event === "attention";

  if (isAttention) {
    copy.textContent = `${event.source === "claude" ? "Claude" : "Codex"} needs your approval.`;
  } else {
    copy.textContent = "Baka! I'm waiting...";
  }

  setStatus("ready");
  bubble.hidden = false;
  bubble.classList.remove("arriving");
  void bubble.offsetWidth;
  bubble.classList.add("arriving");

  timeout = window.setTimeout(hideBubble, isAttention ? 12_000 : 8_000);
}

function hideBubble() {
  window.clearTimeout(timeout);
  bubble.hidden = true;
  setStatus("idle");
}

function setStatus(status: PipStatus) {
  image.src = imageByStatus[status];
  image.alt = altByStatus[status];
  pip.dataset.status = status;
  pip.classList.toggle("is-alert", status === "ready");
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

setStatus("idle");
void listen<PipEvent>("pip:event", ({ payload }) => showEvent(payload));
void invoke<PipEvent | null>("initial_event").then((event) => event && showEvent(event));
