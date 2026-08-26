import { z } from "zod";
import { Uuid } from "./helpers.ts";

/** A project of one organization. */
export const Project = z.strictObject({
  id: Uuid,
  organization_id: Uuid,
  name: z.string(),
  slug: z.string(),
});
export type Project = z.infer<typeof Project>;

/** A repository registered under a project. */
export const Repository = z.strictObject({
  id: Uuid,
  organization_id: Uuid,
  project_id: Uuid,
  remote_url: z.string(),
  default_branch: z.string(),
  display_name: z.string(),
});
export type Repository = z.infer<typeof Repository>;
