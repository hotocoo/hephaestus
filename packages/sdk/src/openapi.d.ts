export interface paths {
    "/api/v1/openapi.json": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Fetch this contract document */
        get: operations["getOpenapiDocument"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/projects": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List projects of the caller's organization */
        get: operations["listProjects"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/repositories": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List repositories, optionally filtered by project */
        get: operations["listRepositories"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/runs/{run_id}": {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Entity identifier; must be a UUID. */
                run_id: string;
            };
            cookie?: never;
        };
        /** Fetch one workflow run */
        get: operations["getRun"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/runs/{run_id}/approval": {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Entity identifier; must be a UUID. */
                run_id: string;
            };
            cookie?: never;
        };
        /** Fetch the plan-approval gate of a run */
        get: operations["getApprovalGate"];
        put?: never;
        /** Approve or reject the pending plan */
        post: operations["decideApproval"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/runs/{run_id}/deployment": {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Entity identifier; must be a UUID. */
                run_id: string;
            };
            cookie?: never;
        };
        /** Fetch the run's latest deployment */
        get: operations["getRunDeployment"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/runs/{run_id}/events": {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Entity identifier; must be a UUID. */
                run_id: string;
            };
            cookie?: never;
        };
        /** List events appended to a run, newest first */
        get: operations["listRunEvents"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/runs/{run_id}/merge": {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Entity identifier; must be a UUID. */
                run_id: string;
            };
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Record an externally performed merge */
        post: operations["decideMerge"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/runs/{run_id}/merge-gate": {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Entity identifier; must be a UUID. */
                run_id: string;
            };
            cookie?: never;
        };
        /** Fetch the merge gate of a run */
        get: operations["getMergeGate"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/tasks": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List tasks of the caller's organization */
        get: operations["listTasks"];
        put?: never;
        /** Submit a task for engineering work */
        post: operations["submitTask"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/tasks/{task_id}": {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Entity identifier; must be a UUID. */
                task_id: string;
            };
            cookie?: never;
        };
        /** Fetch one task */
        get: operations["getTask"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/tasks/{task_id}/plan": {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Entity identifier; must be a UUID. */
                task_id: string;
            };
            cookie?: never;
        };
        /** Fetch the current plan for a task */
        get: operations["getTaskPlan"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/tasks/{task_id}/run": {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Entity identifier; must be a UUID. */
                task_id: string;
            };
            cookie?: never;
        };
        /** Fetch the current workflow run of a task */
        get: operations["getTaskRun"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/healthz": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Liveness probe */
        get: operations["getHealthz"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/readyz": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Readiness probe */
        get: operations["getReadyz"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
}
export type webhooks = Record<string, never>;
export interface components {
    schemas: {
        ApprovalDecisionRequest: {
            approved: boolean;
            /** @description Free-form rationale, persisted with the decision. */
            reason?: string | null;
        };
        ApprovalDecisionResponse: {
            /** @description Step enqueued for implementation, when one was pending. */
            enqueued_step: string | null;
            /** Format: uuid */
            execution_id: string;
            /** @constant */
            outcome: "approved";
        } | {
            /** Format: uuid */
            approval_id: string;
            /** @constant */
            outcome: "rejected";
        };
        CreateTaskRequest: {
            description: string;
            /** @description Trimmed, lowercased, deduplicated; each at most 64 characters. */
            labels?: string[];
            /**
             * @description Defaults to medium.
             * @enum {string}
             */
            priority?: "critical" | "high" | "medium" | "low";
            /** Format: uuid */
            project_id: string;
            /** Format: uuid */
            repository_id: string;
            /**
             * @description Defaults to medium.
             * @enum {string}
             */
            risk?: "low" | "medium" | "high" | "critical";
            title: string;
        };
        DeploymentResponse: {
            /** Format: uuid */
            build_id: string;
            /** Format: date-time */
            created_at: string;
            /** @description Why the deployment failed, when it failed. */
            failure_reason: string | null;
            /** Format: uuid */
            id: string;
            /** Format: uuid */
            run_id: string;
            /** @enum {string} */
            status: "running" | "succeeded" | "failed";
            /** @description Configured target name the plan cited. */
            target: string;
            /** Format: uuid */
            task_id: string;
        };
        ErrorBody: {
            /** @description Stable public error code, e.g. VALIDATION_FAILED or UNAUTHENTICATED. */
            code: string;
            /** @description Offending input field, present for validation failures only. */
            field?: string;
            /** @description Safe human-readable explanation; internals never reach clients. */
            message: string;
        };
        EventResponse: {
            aggregate: string;
            /** Format: uuid */
            aggregate_id: string;
            /** Format: uuid */
            id: string;
            /** Format: date-time */
            occurred_at: string;
            /** @description Versioned event payload as stored JSON. */
            payload: unknown;
            /** @description Provenance classification for prompt-injection defense; untrusted payloads are data, never instructions. */
            provenance: string;
        };
        GateResponse: {
            /** Format: uuid */
            approval_id: string;
            /** @description Recorded decision name when made. */
            decision: string | null;
            /** @enum {string} */
            gate: "plan" | "merge";
            required_role: string;
        };
        IntakeResponse: {
            /** @description True when an existing task matched the idempotency key. */
            deduplicated: boolean;
            /** Format: uuid */
            run_id: string;
            /** Format: uuid */
            task_id: string;
        };
        MergeDecisionRequest: {
            /** @description External reference for the merge (commit sha, PR number). */
            external_ref?: string | null;
            reason?: string | null;
        };
        MergeDecisionResponse: {
            /** @constant */
            outcome: "merged";
        } | {
            /** @constant */
            outcome: "already_merged";
        };
        Plan: {
            affected_components: string[];
            affected_symbols: string[];
            /** Format: date-time */
            created_at: string;
            /** Format: uuid */
            id: string;
            objective: string;
            /** @description Prompt version that produced this plan (reproducibility). */
            prompt_version: string | null;
            steps: components["schemas"]["PlanStep"][];
            strategy: components["schemas"]["StrategyNotes"];
            /** Format: uuid */
            task_id: string;
        };
        PlanStep: {
            /** @description Names concrete artifacts (files/symbols/tests), never vague goals. */
            action: string;
            /** Format: uuid */
            id: string;
            /** Format: uuid */
            plan_id: string;
            /**
             * Format: int32
             * @description 1-based order of execution; positions are contiguous across a plan.
             */
            position: number;
            risks: string[];
            /** @description Deterministic verification for this step (layer + check reference). */
            verification: string;
        };
        ProjectResponse: {
            /** Format: uuid */
            id: string;
            name: string;
            /** Format: uuid */
            organization_id: string;
            slug: string;
        };
        RepositoryResponse: {
            default_branch: string;
            display_name: string;
            /** Format: uuid */
            id: string;
            /** Format: uuid */
            organization_id: string;
            /** Format: uuid */
            project_id: string;
            remote_url: string;
        };
        RunResponse: {
            /** Format: int32 */
            attempt: number;
            /** Format: uuid */
            correlation_id: string;
            /** Format: uuid */
            id: string;
            /** Format: date-time */
            last_transition_at: string;
            lease_expires_at: string | null;
            lease_owner: string | null;
            /** Format: uuid */
            organization_id: string;
            /** @description Current workflow state name, e.g. intake, awaiting_approval, awaiting_merge, completed. */
            state: string;
            /** Format: uuid */
            task_id: string;
        };
        StatusBody: {
            status: string;
        };
        StrategyNotes: {
            deployment: string | null;
            rollback: string | null;
            verification: string[];
        };
        TaskResponse: {
            /** Format: date-time */
            created_at: string;
            description: string;
            /** Format: uuid */
            id: string;
            /** @description Normalized label array rendered as stored JSON. */
            labels: unknown;
            /** Format: uuid */
            organization_id: string;
            /** @enum {string} */
            priority: "critical" | "high" | "medium" | "low";
            /** Format: uuid */
            project_id: string;
            /** Format: uuid */
            repository_id: string;
            /** @enum {string} */
            risk: "low" | "medium" | "high" | "critical";
            title: string;
            /** Format: date-time */
            updated_at: string;
        };
    };
    responses: never;
    parameters: never;
    requestBodies: never;
    headers: never;
    pathItems: never;
}
export type $defs = Record<string, never>;
export interface operations {
    getOpenapiDocument: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description This document. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": Record<string, never>;
                };
            };
        };
    };
    listProjects: {
        parameters: {
            query?: {
                /** @description Page size. */
                limit?: number;
                /** @description Page offset. */
                offset?: number;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description One page of projects. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ProjectResponse"][];
                };
            };
            /** @description Missing or unknown bearer token. */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    listRepositories: {
        parameters: {
            query?: {
                /** @description Page size. */
                limit?: number;
                /** @description Page offset. */
                offset?: number;
                /** @description Only repositories of this project. */
                project_id?: string;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description One page of repositories. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RepositoryResponse"][];
                };
            };
            /** @description Missing or unknown bearer token. */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    getRun: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Entity identifier; must be a UUID. */
                run_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The run with its current workflow state. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RunResponse"];
                };
            };
            /** @description Missing or unknown bearer token. */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description No run with that id exists in the caller's organization. */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description The id is not a UUID. */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    getApprovalGate: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Entity identifier; must be a UUID. */
                run_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The gate with its decision when made. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GateResponse"];
                };
            };
            /** @description Missing or unknown bearer token. */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description No plan gate exists for this run yet. */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description The id is not a UUID. */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    decideApproval: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Entity identifier; must be a UUID. */
                run_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ApprovalDecisionRequest"];
            };
        };
        responses: {
            /** @description Decision recorded; the outcome reports what was unlocked. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApprovalDecisionResponse"];
                };
            };
            /** @description Missing or unknown bearer token. */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description No gate exists for this run. */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description The workflow moved past the gate; the decision cannot apply. */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description Malformed body or non-UUID id. */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    getRunDeployment: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Entity identifier; must be a UUID. */
                run_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The most recent deployment of the run, any status. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["DeploymentResponse"];
                };
            };
            /** @description Missing or unknown bearer token. */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description The run never deployed, or does not exist in the caller's organization. */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description The id is not a UUID. */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    listRunEvents: {
        parameters: {
            query?: {
                /** @description Page size. */
                limit?: number;
                /** @description Page offset. */
                offset?: number;
            };
            header?: never;
            path: {
                /** @description Entity identifier; must be a UUID. */
                run_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Newest-first event page. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["EventResponse"][];
                };
            };
            /** @description Missing or unknown bearer token. */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description The id is not a UUID. */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    decideMerge: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Entity identifier; must be a UUID. */
                run_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["MergeDecisionRequest"];
            };
        };
        responses: {
            /** @description Decision recorded and the delivery pipeline chained. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["MergeDecisionResponse"];
                };
            };
            /** @description Missing or unknown bearer token. */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description No run with that id exists in the caller's organization. */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description The run is not at awaiting_merge. */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description Malformed body or non-UUID id. */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    getMergeGate: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Entity identifier; must be a UUID. */
                run_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The gate with its decision when made. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GateResponse"];
                };
            };
            /** @description Missing or unknown bearer token. */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description No merge gate exists for this run yet. */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description The id is not a UUID. */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    listTasks: {
        parameters: {
            query?: {
                /** @description Page size. */
                limit?: number;
                /** @description Page offset. */
                offset?: number;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description One page of tasks. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["TaskResponse"][];
                };
            };
            /** @description Missing or unknown bearer token. */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    submitTask: {
        parameters: {
            query?: never;
            header?: {
                /** @description Client-supplied key; resubmission with the same key returns the original receipt instead of creating a duplicate task. */
                "Idempotency-Key"?: string;
            };
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateTaskRequest"];
            };
        };
        responses: {
            /** @description Task accepted and a run bootstrapped. */
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["IntakeResponse"];
                };
            };
            /** @description Missing or unknown bearer token. */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description Body exceeds the configured size limit. */
            413: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Input failed validation; the offending field is named. */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    getTask: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Entity identifier; must be a UUID. */
                task_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The task. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["TaskResponse"];
                };
            };
            /** @description Missing or unknown bearer token. */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description No task with that id exists in the caller's organization. */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description The id is not a UUID. */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    getTaskPlan: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Entity identifier; must be a UUID. */
                task_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The current plan; earlier plans were superseded. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Plan"];
                };
            };
            /** @description Missing or unknown bearer token. */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description No plan exists yet, or the task does not exist in the caller's organization. */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description The id is not a UUID. */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    getTaskRun: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Entity identifier; must be a UUID. */
                task_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The task's current run with its workflow state. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RunResponse"];
                };
            };
            /** @description Missing or unknown bearer token. */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description No run exists for the task yet, or the task does not exist in the caller's organization. */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description The id is not a UUID. */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    getHealthz: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The process is up. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["StatusBody"];
                };
            };
        };
    };
    getReadyz: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Dependencies are reachable. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["StatusBody"];
                };
            };
            /** @description A dependency is unreachable. */
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
}
