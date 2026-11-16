// The token a page was opened with: in the link after `#token=`, which
// browsers never send to a server. Read once, then kept for this tab only
// so a reload still works, and removed from the address bar.

declare const brand: unique symbol;

/** A doppel token as `doppel daemon token` prints it: `dpl_` and 43 base64url characters. */
export type Token = string & { readonly [brand]: "Token" };

const SHAPE = /^dpl_[A-Za-z0-9_-]{43}$/;

/** A token, if `raw` has a token's shape. The daemon decides if it's valid. */
export function parseToken(raw: string): Token | null {
  return SHAPE.test(raw) ? (raw as Token) : null;
}

/** The token in a URL's fragment, and the fragment without it. */
export function tokenFromFragment(fragment: string): { token: Token | null; rest: string } {
  const params = new URLSearchParams(fragment.replace(/^#/, ""));
  const raw = params.get("token");
  params.delete("token");
  const rest = params.toString();
  return { token: raw === null ? null : parseToken(raw), rest: rest === "" ? "" : `#${rest}` };
}

/**
 * The `document.cookie` assignment that gives recordings' videos the token,
 * or with `null` takes it away: a `<video>` can't send a header, so the
 * gateway reads the token from this cookie. Only for `/recordings/`, only
 * over HTTPS, never sent from another site's page, and gone when the
 * browser quits.
 */
export function tokenCookie(token: Token | null): string {
  const scope = "Path=/recordings/; Secure; SameSite=Strict";
  return token === null ? `doppel-token=; Max-Age=0; ${scope}` : `doppel-token=${token}; ${scope}`;
}
