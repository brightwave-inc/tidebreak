import rehypeKatex from "rehype-katex";
import "katex/dist/katex.min.css";

/** Isolated so KaTeX CSS and the renderer load only when a block contains `$`. */
export default rehypeKatex;
