import fs from 'node:fs';
import path from 'node:path';
import matter from 'gray-matter';

const CONTENT_DIR = path.join(process.cwd(), 'content', 'docs');
const OUTPUT_DIR = path.join(process.cwd(), 'public');
const OUTPUT_PATH = path.join(OUTPUT_DIR, 'search-index.json');

// The scaffold has to build before any content exists, so a missing or empty
// content directory produces an empty index rather than failing the build.
const files = fs.existsSync(CONTENT_DIR)
  ? fs.readdirSync(CONTENT_DIR).filter((f) => f.endsWith('.mdx'))
  : [];

const index = files.map((file) => {
  const raw = fs.readFileSync(path.join(CONTENT_DIR, file), 'utf-8');
  const { data, content } = matter(raw);
  const basename = path.basename(file, '.mdx');
  const slug = basename === 'index' ? '' : basename;

  return {
    title: data.title ?? basename,
    description: data.description ?? '',
    url: slug ? `/${slug}` : '/',
    content: content
      .replace(/^import\s.+$/gm, '')
      .replace(/<[^>]+>/g, '')
      .replace(/[#*`\[\]]/g, '')
      .slice(0, 2000),
  };
});

fs.mkdirSync(OUTPUT_DIR, { recursive: true });
fs.writeFileSync(OUTPUT_PATH, JSON.stringify(index));
console.log(`Search index: ${index.length} pages → ${OUTPUT_PATH}`);
