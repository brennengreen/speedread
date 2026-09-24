import { x } from "./x";

/** Options for the server. */
export interface Options {
  port: number;
  host?: string;
  handler(req: Request): Promise<Response>;
}

export type Id = string | number;

export enum Color {
  Red,
  Green,
}

export class Server<T> extends Base implements Runnable {
  private port: number;

  constructor(opts: Options) {
    super();
    this.port = opts.port;
    this.init();
  }

  async start(): Promise<void> {
    await this.listen(this.port);
    console.log("started");
    return;
  }

  handle = async (req: Request) => {
    const body = await req.text();
    const out = body.trim();
    return new Response(out);
  };
}

export function createServer(opts: Options): Server<unknown> {
  const s = new Server(opts);
  s.start();
  return s;
}

export const helper = (a: number, b: number): number => {
  const c = a + b;
  const d = c * 2;
  return d;
};

export default function main() {
  const s = createServer({ port: 1 } as Options);
  s.start();
  return s;
}

namespace Utils {
  export function clamp(x: number) {
    const y = Math.max(0, x);
    const z = Math.min(1, y);
    return z;
  }
}

describe("Server", () => {
  it("starts", async () => {
    const s = createServer({ port: 1 } as Options);
    await s.start();
    expect(s).toBeTruthy();
  });
});
