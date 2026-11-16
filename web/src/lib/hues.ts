// The sessions' colours over a machine's screen: an agent's pointer and the
// marks its calls leave in `fill`, a soft wash, and its captions in white on
// `ink`. The same in light and dark: they are drawn over the guest's screen.
// The desktop viewer has the same palette (`theme::Hue`).

export interface Hue {
  fill: string;
  soft: string;
  ink: string;
}

/**
 * Fill and ink, as hex. The first is the agent's colour of one session alone.
 * None is the signal orange: that means a person in control, or attention.
 */
export const HUES: readonly (readonly [string, string])[] = [
  ["#22e5ff", "#0b5d6b"], // cyan
  ["#3d8bff", "#1f4f9e"], // blue
  ["#b18cff", "#5b3aa8"], // violet
  ["#c3d94e", "#566314"], // lime
];

/** Session `session`'s colours: sessions count from 1, and a recording's one session (0) takes the first. */
export function hue(session: number): Hue {
  const [fill, ink] = HUES[Math.max(0, session - 1) % HUES.length] ?? HUES[0]!;
  return { fill, soft: `${fill}38`, ink };
}

/** WCAG contrast between two hex colours. */
export function contrast(a: string, b: string): number {
  const luminance = (hex: string) => {
    const value = Number.parseInt(hex.slice(1), 16);
    const channel = (shift: number) => {
      const v = ((value >> shift) & 0xff) / 255;
      return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
    };
    return 0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0);
  };
  const [x, y] = [luminance(a), luminance(b)];
  return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
}
