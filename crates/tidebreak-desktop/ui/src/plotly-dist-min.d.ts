/**
 * `plotly.js-dist-min` is the prebuilt bundle — building plotly.js from source
 * needs a bundling step we do not want — and it ships no declarations of its
 * own. Its runtime surface is the `plotly.js` namespace, whose published types
 * we carry as a devDependency, exposed as one default export.
 */
declare module "plotly.js-dist-min" {
  import * as Plotly from "plotly.js";
  const plotly: typeof Plotly;
  export default plotly;
}
