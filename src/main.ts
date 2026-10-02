import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { invoke } from "@tauri-apps/api/core";
import "./style.css";

type PipEvent = {
  source: "codex" | "claude" | "test" | string;
  event: "idle" | "thinking" | "ready" | "attention" | string;
  session?: string;
};

type PipStatus = "idle" | "thinking" | "ready";

type PendingReminder = {
  kind: "water" | "eyes" | string;
  message: string;
  time: string;
};

const pip = document.querySelector<HTMLElement>("#pip")!;
const image = document.querySelector<HTMLImageElement>("#pip-image")!;
const bubble = document.querySelector<HTMLElement>("#bubble")!;
const copy = document.querySelector<HTMLElement>("#bubble-copy")!;
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
let currentStatus: PipStatus = "idle";
const busySessions = new Set<string>();
let bubbleSession: string | undefined;
let queuedReadySession: string | undefined;
let pendingReminders: PendingReminder[] = [];
let shownReminder: PendingReminder | undefined;
let ghostModeEnabled = false;
let clickThrough = false;

function setGhostMode(enabled: boolean) {
  ghostModeEnabled = enabled;
  updateGhostAppearance();
  ghostToggle.textContent = enabled ? "Solid" : "Fade";
  ghostToggle.setAttribute("aria-pressed", String(enabled));
  ghostToggle.setAttribute(
    "aria-label",
    enabled ? "Use full opacity" : "Use ghost mode",
  );
}

function updateGhostAppearance() {
  const faded = ghostModeEnabled && shownReminder === undefined && bubbleSession === undefined;
  pip.classList.toggle("is-ghost", faded);
  if (faded !== clickThrough) {
    clickThrough = faded;
    if (faded) resetHoverControls();
    void getCurrentWindow().setIgnoreCursorEvents(faded);
  }
}

function resetHoverControls() {
  (document.activeElement as HTMLElement | null)?.blur();
  pip.classList.remove("has-pointer-activity");
  pip.addEventListener("pointermove", enableHoverControls);
}

function showEvent(event: PipEvent) {
  const session = `${event.source}:${event.session ?? ""}`;
  if (event.event === "thinking") {
    busySessions.add(session);
  } else {
    busySessions.delete(session);
  }

  if (event.event === "thinking" || event.event === "idle") {
    if (queuedReadySession === session) queuedReadySession = undefined;
    if (shownReminder !== undefined) return;
    if (bubbleSession === session) {
      hideBubble();
    } else if (bubbleSession === undefined) {
      showAgentActivity();
    }
    return;
  }

  if (shownReminder !== undefined) {
    queuedReadySession = session;
    return;
  }
  showReadyBubble(session);
}

function showReadyBubble(session: string) {
  bubbleSession = session;
  updateGhostAppearance();
  showBubble("Baka! I'm waiting...", 10_000);
}

function setPendingReminders(reminders: PendingReminder[]) {
  pendingReminders = reminders;
  if (shownReminder !== undefined) {
    const shownKind = shownReminder.kind;
    const stillPending = reminders.find((reminder) => reminder.kind === shownKind);
    if (stillPending !== undefined) {
      shownReminder = stillPending;
    } else {
      hideBubble();
    }
    return;
  }
  if (reminders[0] !== undefined) showReminder(reminders[0]);
}

function showReminder(reminder: PendingReminder) {
  if (bubbleSession !== undefined) queuedReadySession = bubbleSession;
  bubbleSession = undefined;
  shownReminder = reminder;
  updateGhostAppearance();
  showBubble(reminder.message);
}

function dismissBubble() {
  if (shownReminder !== undefined) {
    const { kind, time } = shownReminder;
    pendingReminders = pendingReminders.filter((reminder) => reminder.kind !== kind);
    void invoke("dismiss_reminder", { kind, time });
  }
  hideBubble();
}

function showBubble(message: string, hideAfterMs?: number) {
  window.clearTimeout(timeout);
  timeout = undefined;
  copy.textContent = message;

  setStatus("ready");
  bubble.hidden = false;
  bubble.classList.remove("arriving");
  void bubble.offsetWidth;
  bubble.classList.add("arriving");

  if (hideAfterMs !== undefined) {
    timeout = window.setTimeout(hideBubble, hideAfterMs);
  }
}

function hideBubble() {
  window.clearTimeout(timeout);
  timeout = undefined;
  bubbleSession = undefined;
  shownReminder = undefined;
  updateGhostAppearance();
  bubble.hidden = true;

  const nextReminder = pendingReminders[0];
  if (nextReminder !== undefined) {
    showReminder(nextReminder);
    return;
  }
  if (queuedReadySession !== undefined) {
    const session = queuedReadySession;
    queuedReadySession = undefined;
    showReadyBubble(session);
    return;
  }
  showAgentActivity();
}

function showAgentActivity() {
  setStatus(busySessions.size > 0 ? "thinking" : "idle");
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
  const wasThinking = currentStatus === "thinking";
  currentStatus = status;

  if (status === "thinking") {
    if (!wasThinking) startThinkingAnimation();
  } else {
    stopThinkingAnimation();
    image.src = imageByStatus[status];
  }
  image.alt = altByStatus[status];
  pip.dataset.status = status;
  pip.classList.toggle("is-alert", status === "ready");
}

ghostToggle.addEventListener("pointerdown", (event) => {
  event.stopPropagation();
  void invoke("toggle_ghost_mode");
});

hidePip.addEventListener("pointerdown", (event) => {
  event.stopPropagation();
  resetHoverControls();
  void invoke("hide_window");
});

function enableHoverControls(event: PointerEvent) {
  if (event.movementX === 0 && event.movementY === 0) return;
  pip.classList.add("has-pointer-activity");
  pip.removeEventListener("pointermove", enableHoverControls);
}

pip.addEventListener("pointermove", enableHoverControls);

pip.addEventListener("pointerdown", async (event) => {
  if ((event.target as HTMLElement).closest("button")) return;
  if (currentStatus === "ready") dismissBubble();
  await getCurrentWindow().startDragging();
});

setStatus("idle");
void listen<boolean>("pip:ghost", ({ payload }) => setGhostMode(payload));
void listen<PipEvent>("pip:event", ({ payload }) => showEvent(payload));
void listen<PendingReminder[]>("pip:reminders", ({ payload }) => setPendingReminders(payload));
void invoke<boolean>("ghost_mode").then(setGhostMode);
void invoke<PipEvent | null>("initial_event")
  .then((event) => event && showEvent(event))
  .then(() => invoke<PendingReminder[]>("pending_reminders"))
  .then(setPendingReminders);
