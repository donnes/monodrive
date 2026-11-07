// Springs in Apple's terms: `response` is roughly the seconds to reach the
// target, `damping` 1 settles without overshoot and lower values bounce.
// They animate from the current value and keep their velocity when the target
// moves, so any motion built on them can be grabbed and reversed.

export type SpringOptions = {
  response?: number;
  damping?: number;
  /** How close counts as settled, in the value's own units. */
  precision?: number;
};

export class Spring {
  value: number;
  target: number;
  velocity = 0;
  private stiffness = 0;
  private friction = 0;
  private precision: number;

  constructor(value: number, options: SpringOptions = {}) {
    this.value = value;
    this.target = value;
    this.precision = options.precision ?? 0.01;
    this.configure(options);
  }

  configure({ response = 0.4, damping = 1 }: SpringOptions) {
    this.stiffness = ((2 * Math.PI) / response) ** 2;
    this.friction = (4 * Math.PI * damping) / response;
  }

  set(value: number) {
    this.value = value;
    this.target = value;
    this.velocity = 0;
  }

  step(dt: number) {
    // Fixed substeps keep stiff springs stable on 30 Hz and 120 Hz frames alike.
    let left = Math.min(dt, 1 / 20);
    while (left > 0) {
      const h = Math.min(1 / 240, left);
      const force = -this.stiffness * (this.value - this.target) - this.friction * this.velocity;
      this.velocity += force * h;
      this.value += this.velocity * h;
      left -= h;
    }
    if (this.settled) {
      this.value = this.target;
      this.velocity = 0;
    }
  }

  get settled() {
    return (
      Math.abs(this.value - this.target) < this.precision &&
      Math.abs(this.velocity) < this.precision * 10
    );
  }
}

/** Runs `tick` each frame while it returns true, and stops when it returns false.
 *  Nothing repaints while every spring is at rest. */
export class FrameLoop {
  private frame = 0;
  private last = 0;

  constructor(private tick: (dt: number) => boolean) {}

  start() {
    if (this.frame) return;
    this.last = performance.now();
    const run = (now: number) => {
      const dt = (now - this.last) / 1000;
      this.last = now;
      this.frame = this.tick(dt) ? requestAnimationFrame(run) : 0;
    };
    this.frame = requestAnimationFrame(run);
  }
}

export const reducedMotion = () => matchMedia("(prefers-reduced-motion: reduce)").matches;
