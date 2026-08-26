<script setup lang="ts">
/**
 * Fail-closed boot screen.
 *
 * Without a pre-provisioned bearer token bound to one organization
 * the dashboard makes no requests at all - there is no interactive
 * login because the API defines none. This screen tells the operator
 * exactly how to configure credentials.
 */
import { PhFlame } from "@phosphor-icons/vue";
</script>

<template>
  <div class="setup panel">
    <h1><PhFlame :size="24" weight="fill" style="color:var(--accent)" aria-hidden="true" /> Hephaestus</h1>
    <p style="margin:14px 0 8px">
      This dashboard has no API credentials, so it will not talk to the
      control plane. Requests authenticate with pre-provisioned bearer
      keys bound to exactly one organization; there is no interactive login.
    </p>
    <p style="color:var(--text-muted)">
      To deploy the dashboard, set the runtime configuration in index.html:
    </p>
<pre>window.__HEPHAESTUS_WEB_CONFIG__ = {
  baseUrl: "",            // control-plane origin; empty means same-origin
  token: "your-api-key", // required: a key provisioned on the server
  pollSeconds: 10,       // optional refresh interval
};</pre>
    <ol style="margin-top:16px">
      <li>Provision an API key in the server's [auth.keys] configuration.</li>
      <li>Inject the configuration above into the served HTML (or edit index.html for development).</li>
      <li>Reload this page.</li>
    </ol>
  </div>
</template>
