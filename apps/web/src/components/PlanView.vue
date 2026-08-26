<script setup lang="ts">
/**
 * A plan: objective, ordered steps with their deterministic
 * verification hooks, affected surface and strategy notes.
 */
import type { Plan } from "@hephaestus/sdk";
import { formatDateTime } from "@/lib/format";

defineProps<{ plan: Plan | null }>();
</script>

<template>
  <p v-if="plan === null" class="cell-sub" style="padding:16px">
    no plan generated yet - planning produces one after analysis
  </p>
  <div v-else class="panel__body">
    <p style="margin-bottom:14px; max-width:72ch">{{ plan.objective }}</p>
    <div class="steps">
      <div v-for="step in plan.steps" :key="step.id" class="step">
        <span class="step__pos" aria-hidden="true"></span>
        <div>
          <p class="step__action">{{ step.action }}</p>
          <p class="step__verify">
            <span>verify:</span>
            <code class="mono">{{ step.verification }}</code>
          </p>
          <div v-if="step.risks.length > 0" class="risks">
            <span v-for="risk in step.risks" :key="risk" class="pill pill--warn">{{ risk }}</span>
          </div>
        </div>
      </div>
    </div>
    <dl class="kv" style="margin-top:16px">
      <dt>Affected components</dt>
      <dd>
        <span v-if="plan.affected_components.length === 0" class="cell-sub">none named</span>
        <span v-else class="chips">
          <code v-for="c in plan.affected_components" :key="c">{{ c }}</code>
        </span>
      </dd>
      <dt>Affected symbols</dt>
      <dd>
        <span v-if="plan.affected_symbols.length === 0" class="cell-sub">none named</span>
        <span v-else class="chips">
          <code v-for="s in plan.affected_symbols" :key="s">{{ s }}</code>
        </span>
      </dd>
      <dt>Rollback</dt>
      <dd>{{ plan.strategy.rollback ?? "not stated" }}</dd>
      <dt>Deployment</dt>
      <dd>{{ plan.strategy.deployment ?? "not stated" }}</dd>
      <dt>Verification layers</dt>
      <dd>
        <span v-if="plan.strategy.verification.length === 0" class="cell-sub">none</span>
        <ul v-else style="margin:0; padding-left:18px">
          <li v-for="v in plan.strategy.verification" :key="v"><code class="mono">{{ v }}</code></li>
        </ul>
      </dd>
      <dt>Generated</dt>
      <dd>{{ formatDateTime(plan.created_at) }}<template v-if="plan.prompt_version"> · prompt {{ plan.prompt_version }}</template></dd>
    </dl>
  </div>
</template>