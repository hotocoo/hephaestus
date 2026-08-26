// Shared flat ESLint configuration.
//
// Scope discipline, not style: formatting belongs to editors
// (.editorconfig). These rules catch real defects and keep imports
// honest across every TypeScript package in the workspace.
//
// @ts-check
import tseslint from "typescript-eslint";

export default tseslint.config(
  { ignores: ["**/dist/**", "**/coverage/**", "**/node_modules/**"] },
  ...tseslint.configs.recommended,
  {
    rules: {
      "@typescript-eslint/no-unused-vars": [
        "error",
        { argsIgnorePattern: "^_", varsIgnorePattern: "^_" },
      ],
      "@typescript-eslint/consistent-type-imports": [
        "error",
        { fixStyle: "inline-type-imports" },
      ],
      "@typescript-eslint/no-non-null-assertion": "error",
    },
  },
);
