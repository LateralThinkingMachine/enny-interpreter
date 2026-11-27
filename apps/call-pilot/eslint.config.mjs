import js from "@eslint/js";
import tseslint from "typescript-eslint";
import globals from "globals";
import reactHooks from "eslint-plugin-react-hooks";
export default tseslint.config({ignores:["dist/**",".test-build/**","src/audio/capture-worklet.js"]},js.configs.recommended,...tseslint.configs.recommended,{files:["src/**/*.{ts,tsx}"],languageOptions:{globals:globals.browser},plugins:{"react-hooks":reactHooks},rules:{...reactHooks.configs.recommended.rules}},{files:["*.mjs","tests/**/*.mjs"],languageOptions:{globals:globals.node}});
