import { z } from "zod";

/** A UUID as rendered by the API (serde transparent identifiers). */
export const Uuid = z.string().uuid();
export type Uuid = z.infer<typeof Uuid>;

/**
 * An RFC 3339 timestamp. Chrono renders UTC with optional fractional
 * seconds; the wire format is a plain string and stays one here so no
 * precision is lost between transport and render.
 */
export const IsoDateTime = z.string().datetime({ offset: false });
export type IsoDateTime = z.infer<typeof IsoDateTime>;
