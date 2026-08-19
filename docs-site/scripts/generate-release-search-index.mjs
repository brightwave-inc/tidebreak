import fs from 'node:fs';
import path from 'node:path';

const outputDirectory = path.join(process.cwd(), 'out');
const llmsPath = path.join(outputDirectory, 'llms.txt');
const searchIndexPath = path.join(outputDirectory, 'search-index.json');

if (!fs.existsSync(llmsPath)) {
  throw new Error(`Static documentation is missing ${llmsPath}`);
}

const pages = fs
  .readFileSync(llmsPath, 'utf8')
  .split('\n')
  .flatMap((line) => {
    const match = /^- \[([^\]]+)]\(([^)]+)\):\s*(.+)$/.exec(line);
    if (!match) return [];

    const [, title, markdownUrl, description] = match;
    const url = markdownUrl
      .replace(/\/llms-mdx\/index$/, '/')
      .replace('/llms-mdx/', '/');

    return [{ title, description, url, content: description }];
  });

if (pages.length === 0) {
  throw new Error(`No documentation pages were found in ${llmsPath}`);
}

fs.writeFileSync(searchIndexPath, JSON.stringify(pages));
console.log(`Release search manifest: ${pages.length} pages → ${searchIndexPath}`);
