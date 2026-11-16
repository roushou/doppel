/**
 * Which theme the viewer shows: the system's appearance, or light or dark
 * whatever the system says. The person picks it; it's kept across loads.
 * The desktop viewer has the same (`ThemeChoice` in doppel-viewer).
 */
export type ThemeChoice = "system" | "light" | "dark";

/** The themes there are to show. */
export type Shown = "light" | "dark";

/** A kept choice, from storage: anything unrecognized is the system's. */
export function readTheme(raw: string | null): ThemeChoice {
  switch (raw) {
    case "light":
    case "dark":
    case "system":
      return raw;
    default:
      return "system";
  }
}

/** The next choice round: system, light, dark, and back. */
export function nextTheme(choice: ThemeChoice): ThemeChoice {
  switch (choice) {
    case "system":
      return "light";
    case "light":
      return "dark";
    case "dark":
      return "system";
    default: {
      const never: never = choice;
      return never;
    }
  }
}

/** The theme a choice shows, with the system light or not. */
export function shownTheme(choice: ThemeChoice, systemLight: boolean): Shown {
  switch (choice) {
    case "system":
      return systemLight ? "light" : "dark";
    case "light":
    case "dark":
      return choice;
    default: {
      const never: never = choice;
      return never;
    }
  }
}

/** What the switch says it is, and what a press does. */
export function themeLabel(choice: ThemeChoice): string {
  switch (choice) {
    case "system":
      return "Theme: system (press for light)";
    case "light":
      return "Theme: light (press for dark)";
    case "dark":
      return "Theme: dark (press for system)";
    default: {
      const never: never = choice;
      return never;
    }
  }
}
