import fs from 'fs';
import path from 'path';

const promptsDir = path.join(process.cwd(), 'prompts');
const files = fs.readdirSync(promptsDir).filter(f => f.endsWith('.md') && f !== 'AI_PROMPTS.md');

const contextString = "Context: The user is currently using 'Noland' (or Noland Connect), a platform that automates the process of renting cloud GPUs (via Vast.ai) and seamlessly connecting them for high-end remote game streaming. ";

let count = 0;

for (const file of files) {
  const filePath = path.join(promptsDir, file);
  let content = fs.readFileSync(filePath, 'utf-8');
  
  if (!content.startsWith("Context: The user is currently using 'Noland'")) {
    fs.writeFileSync(filePath, contextString + content);
    count++;
  }
}

console.log(`Updated ${count} prompt files with Noland context.`);
