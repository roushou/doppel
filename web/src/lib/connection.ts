// A connection to the daemon through the gateway's WebSocket: the token
// first, then `doppel.daemon.v1` frames, one per WebSocket message.
// Requests are answered by id; a display's frames come as events of the
// request that opened it.

import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { HelloSchema } from "../gen/doppel/common/v1/common_pb";
import {
  type DaemonEvent,
  DaemonFrameSchema,
  type DaemonRequest,
  type DaemonResponse,
  DaemonResponseSchema,
} from "../gen/doppel/daemon/v1/daemon_pb";
import type { Token } from "./token";

/** The protocol version this page speaks, as the daemon's Hello says. */
const PROTOCOL_VERSION = 1;

export type Operation = DaemonRequest["operation"];
export type Outcome = DaemonResponse["outcome"];

/** Why a connection ended, for the person looking at the page. */
export interface Closed {
  code: number;
  reason: string;
}

interface Pending {
  answered: (response: DaemonResponse) => void;
  event?: (event: DaemonEvent) => void;
}

/** What `Connection.open` failed with. */
export class ConnectError extends Error {
  constructor(readonly closed: Closed) {
    super(closed.reason === "" ? `closed (${closed.code})` : closed.reason);
  }
}

export class Connection {
  readonly #socket: WebSocket;
  readonly #pending = new Map<bigint, Pending>();
  #next = 1n;
  #closed: Closed | null = null;
  readonly #onClose: ((closed: Closed) => void)[] = [];

  private constructor(socket: WebSocket) {
    this.#socket = socket;
  }

  /** The socket at this page's own origin. */
  static socketUrl(location: Location): string {
    const scheme = location.protocol === "https:" ? "wss:" : "ws:";
    return `${scheme}//${location.host}/socket`;
  }

  /** Connects, presents the token, and greets the daemon. */
  static open(url: string, token: Token): Promise<Connection> {
    return new Promise((resolve, reject) => {
      const socket = new WebSocket(url);
      socket.binaryType = "arraybuffer";
      const connection = new Connection(socket);
      let greeted = false;
      socket.onopen = () => {
        socket.send(token);
        connection.#send({
          kind: {
            case: "hello",
            value: create(HelloSchema, {
              protocolVersion: PROTOCOL_VERSION,
              softwareVersion: "doppel web",
            }),
          },
        });
      };
      socket.onmessage = (message: MessageEvent<ArrayBuffer>) => {
        const frame = fromBinary(DaemonFrameSchema, new Uint8Array(message.data));
        switch (frame.kind.case) {
          case "hello":
            if (!greeted) {
              greeted = true;
              resolve(connection);
            }
            return;
          case "response":
            connection.#answered(frame.kind.value);
            return;
          case "event":
            connection.#event(frame.kind.value);
            return;
          case "request":
          case "cancel":
          case undefined:
            return;
          default: {
            const never: never = frame.kind;
            return never;
          }
        }
      };
      socket.onclose = (event) => {
        const closed = { code: event.code, reason: event.reason };
        connection.#closed = closed;
        if (!greeted) reject(new ConnectError(closed));
        // Requests still waiting get an empty outcome.
        for (const pending of connection.#pending.values()) {
          pending.answered(create(DaemonResponseSchema, {}));
        }
        connection.#pending.clear();
        for (const listener of connection.#onClose) listener(closed);
      };
    });
  }

  /** Called once, when the connection ends. */
  onClose(listener: (closed: Closed) => void): void {
    if (this.#closed === null) this.#onClose.push(listener);
    else listener(this.#closed);
  }

  close(): void {
    this.#socket.close(1000);
  }

  #send(frame: Parameters<typeof create<typeof DaemonFrameSchema>>[1]): void {
    if (this.#socket.readyState !== WebSocket.OPEN) return;
    this.#socket.send(toBinary(DaemonFrameSchema, create(DaemonFrameSchema, frame)));
  }

  #answered(response: DaemonResponse): void {
    const pending = this.#pending.get(response.id);
    if (pending === undefined) return;
    // A display's request stays open for its events.
    if (pending.event === undefined) this.#pending.delete(response.id);
    pending.answered(response);
  }

  #event(event: DaemonEvent): void {
    this.#pending.get(event.request)?.event?.(event);
  }

  #request(operation: Operation, caption: string, pending: Omit<Pending, "answered">): Promise<DaemonResponse> {
    const id = this.#next++;
    return new Promise((answered) => {
      this.#pending.set(id, { answered, ...pending });
      this.#send({ kind: { case: "request", value: { id, caption, operation } } });
    });
  }

  /** The outcome of one request; an empty one if the connection closed first. */
  async request(operation: Operation, caption = ""): Promise<Outcome> {
    return (await this.#request(operation, caption, {})).outcome;
  }

  /** A request whose events follow its outcome, such as a display. */
  async stream(operation: Operation, event: (event: DaemonEvent) => void): Promise<Outcome> {
    return (await this.#request(operation, "", { event })).outcome;
  }
}

/** What a request's outcome says went wrong, for a person; or what it was, if it isn't a refusal. */
export function refusal(outcome: Outcome): string {
  switch (outcome.case) {
    case "error": {
      const kind = outcome.value.kind;
      switch (kind.case) {
        case "invalidArgument":
          return `${kind.value.field}: ${kind.value.reason}`;
        case "invalidState":
          return kind.value.reason;
        case "unsupported":
          return kind.value.what;
        case "internal":
          return kind.value.message;
        case "timeout":
          return `timed out after ${kind.value.afterMs} ms`;
        case "permissionMissing":
          return "a permission is missing in the guest";
        case "staleReference":
          return "a stale reference";
        case undefined:
          return "an error this page can't read";
        default: {
          const never: never = kind;
          return never;
        }
      }
    }
    case "machineFailure":
      return outcome.value.message;
    case undefined:
      return "the connection closed";
    default:
      return `an unexpected answer (${outcome.case})`;
  }
}
