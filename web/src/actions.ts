// How each lifecycle action is shown: its words and its icon.

import type { Component } from "svelte";
import {
  Bookmark,
  CircleStop,
  Copy,
  GitFork,
  Pause,
  Play,
  Plus,
  Power,
  RotateCcw,
  Trash2,
  Undo2,
} from "@lucide/svelte";
import type { Action } from "./lib/lifecycle";

export interface ActionLook {
  label: string;
  icon: Component<{ size?: number }>;
  /** It throws work away for good. */
  danger: boolean;
}

export function actionLook(action: Action): ActionLook {
  switch (action) {
    case "start":
      return { label: "Start", icon: Play, danger: false };
    case "start-golden":
      return { label: "Start…", icon: Play, danger: false };
    case "from-template":
      return { label: "New machine from it", icon: Plus, danger: false };
    case "stop":
      return { label: "Shut down", icon: Power, danger: false };
    case "force-stop":
      return { label: "Force stop", icon: CircleStop, danger: true };
    case "pause":
      return { label: "Pause", icon: Pause, danger: false };
    case "resume":
      return { label: "Resume", icon: Play, danger: false };
    case "checkpoint":
      return { label: "Save checkpoint", icon: Bookmark, danger: false };
    case "roll-back":
      return { label: "Roll back…", icon: Undo2, danger: true };
    case "restore":
      return { label: "Restore checkpoint…", icon: RotateCcw, danger: true };
    case "fork":
      return { label: "Fork into…", icon: GitFork, danger: false };
    case "new-copy":
      return { label: "New copy from checkpoint…", icon: Copy, danger: false };
    case "delete":
      return { label: "Delete…", icon: Trash2, danger: true };
    default: {
      const never: never = action;
      return never;
    }
  }
}
