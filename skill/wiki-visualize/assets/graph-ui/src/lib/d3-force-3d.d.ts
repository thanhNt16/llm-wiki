/* Minimal ambient types for d3-force-3d (no @types package exists).
 * Covers only the API this app uses. */
declare module "d3-force-3d" {
  export interface SimulationNodeDatum {
    index?: number;
    x?: number;
    y?: number;
    z?: number;
    vx?: number;
    vy?: number;
    vz?: number;
    fx?: number | null;
    fy?: number | null;
    fz?: number | null;
  }

  export interface Force {
    (alpha: number): void;
    initialize?(nodes: SimulationNodeDatum[]): void;
  }

  export interface ForceSimulation<N> {
    force(name: string, force?: Force): this;
    tick(iterations?: number): this;
    stop(): this;
    nodes(): N[];
  }

  export interface ForceLink {
    (alpha: number): void;
    distance(d: number | ((link: any) => number)): ForceLink;
    strength(s: number | ((link: any) => number)): ForceLink;
  }
  export interface ForceManyBody {
    (alpha: number): void;
    strength(s: number | ((node: any) => number)): ForceManyBody;
  }
  export interface ForceCollide {
    (alpha: number): void;
  }
  export interface ForceAxis {
    (alpha: number): void;
    strength(s: number): ForceAxis;
  }

  export function forceSimulation<N extends SimulationNodeDatum>(
    nodes?: N[],
    numDimensions?: number,
  ): ForceSimulation<N>;

  export function forceLink(links?: unknown[]): ForceLink;
  export function forceManyBody(): ForceManyBody;
  export function forceCollide(
    radius?: number | ((node: any) => number),
  ): ForceCollide;
  export function forceX(x?: number): ForceAxis;
  export function forceY(y?: number): ForceAxis;
  export function forceZ(z?: number): ForceAxis;
}
