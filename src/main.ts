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
const ghostToggle = document.querySelector<HTMLButtonElement>("#ghost-toggle")!;
const hidePip = document.querySelector<HTMLButtonElement>("#hide-pip")!;

const imageByStatus: Record<PipStatus, string> = {
  idle: new URL("./assets/pip-idle.png", import.meta.url).href,
  thinking: new URL("./assets/pip-thinking.png", import.meta.url).href,
  ready: new URL("./assets/pip-ready.png", import.meta.url).href,
};

const thinkingImages = [
  imageByStatus.thinking,
  new URL("./assets/pip-thinking2.png", import.meta.url).href,
  new URL("./assets/pip-thinking3.png", import.meta.url).href,
];

const altByStatus: Record<PipStatus, string> = {
  idle: "Pip-chan is idle",
  thinking: "Pip-chan is thinking",
  ready: "Pip-chan is waiting",
};

let timeout: number | undefined;
let thinkingInterval: number | undefined;
let thinkingImageIndex = 0;

function setGhostMode(enabled: boolean) {
  pip.classList.toggle("is-ghost", enabled);
  ghostToggle.textContent = enabled ? "Solid" : "Fade";
  ghostToggle.setAttribute("aria-pressed", String(enabled));
  ghostToggle.setAttribute(
    "aria-label",
    enabled ? "Use full opacity" : "Use ghost mode",
  );
}

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

  timeout = window.setTimeout(hideBubble, 3_000);
}

function hideBubble() {
  window.clearTimeout(timeout);
  bubble.hidden = true;
  setStatus("idle");
}

function stopThinkingAnimation() {
  window.clearInterval(thinkingInterval);
  thinkingInterval = undefined;
}

function startThinkingAnimation() {
  stopThinkingAnimation();
  thinkingImageIndex = 0;
  image.src = thinkingImages[thinkingImageIndex];
  thinkingInterval = window.setInterval(() => {
    thinkingImageIndex = (thinkingImageIndex + 1) % thinkingImages.length;
    image.src = thinkingImages[thinkingImageIndex];
  }, 3_000);
}

function setStatus(status: PipStatus) {
  if (status === "thinking") {
    startThinkingAnimation();
  } else {
    stopThinkingAnimation();
    image.src = imageByStatus[status];
  }
  image.alt = altByStatus[status];
  pip.dataset.status = status;
  pip.classList.toggle("is-alert", status === "ready");
}

dismiss.addEventListener("pointerdown", (event) => {
  event.stopPropagation();
  hideBubble();
});

ghostToggle.addEventListener("pointerdown", (event) => {
  event.stopPropagation();
  void invoke("toggle_ghost_mode");
});

hidePip.addEventListener("pointerdown", (event) => {
  event.stopPropagation();
  void invoke("hide_window");
});

pip.addEventListener("pointerdown", async (event) => {
  if ((event.target as HTMLElement).closest("button")) return;
  await getCurrentWindow().startDragging();
});

setStatus("idle");
void listen<boolean>("pip:ghost", ({ payload }) => setGhostMode(payload));
void listen<PipEvent>("pip:event", ({ payload }) => showEvent(payload));
void invoke<boolean>("ghost_mode").then(setGhostMode);
void invoke<PipEvent | null>("initial_event").then((event) => event && showEvent(event));
