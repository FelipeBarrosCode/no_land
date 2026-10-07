#!/usr/bin/env node
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, resolve } from "node:path";
import ts from "typescript";

const root = resolve(new URL("..", import.meta.url).pathname);
const localeDir = join(root, "src", "locales");
const source = JSON.parse(readFileSync(join(localeDir, "en-US.json"), "utf8"));
const sourceKeys = new Set(Object.keys(source));
const requiredCompleteLocales = new Set([
  "pt-BR.json", "es.json", "fr.json", "de.json", "it.json",
  "ja.json", "ko.json", "zh-CN.json",
]);
const variablePattern = /\{(\w+)(?:,|\})/g;
let failed = false;

function sourceFiles(directory) {
  return readdirSync(directory).flatMap((name) => {
    const path = join(directory, name);
    if (statSync(path).isDirectory()) return sourceFiles(path);
    return /\.tsx?$/.test(path) ? [path] : [];
  });
}

const localizedAttributes = new Set([
  "title", "placeholder", "aria-label", "alt", "data-text", "loadingText",
  "label", "description", "topic",
]);
const userMessageSetters = new Set([
  "setError", "setLoadError", "setDisconnectError", "setMessage",
]);

function visibleText(value) {
  return /[A-Za-zÀ-ÿ]/u.test(value.replace(/\s+/g, " ").trim());
}

for (const path of sourceFiles(join(root, "src"))) {
  if (path.endsWith("i18n.tsx")) continue;
  const text = readFileSync(path, "utf8");
  const file = ts.createSourceFile(
    path,
    text,
    ts.ScriptTarget.Latest,
    true,
    path.endsWith(".tsx") ? ts.ScriptKind.TSX : ts.ScriptKind.TS,
  );
  const report = (node, message) => {
    const { line } = file.getLineAndCharacterOfPosition(node.getStart(file));
    console.error(`${path}:${line + 1}: ${message}`);
    failed = true;
  };
  const visit = (node) => {
    if (ts.isCallExpression(node)) {
      const name = node.expression.getText(file);
      const first = node.arguments[0];
      if ((name === "t" || name === "translate") && first && ts.isStringLiteral(first)
        && !sourceKeys.has(first.text)) {
        report(first, `unknown localization key ${first.text}`);
      }
      if (userMessageSetters.has(name) && first && ts.isStringLiteral(first) && visibleText(first.text)) {
        report(first, "hardcoded user-facing state message");
      }
    }
    if (ts.isJsxText(node) && visibleText(node.getText(file))) {
      report(node, "hardcoded visible JSX text");
    }
    if (ts.isJsxAttribute(node) && localizedAttributes.has(node.name.getText(file))
      && node.initializer && ts.isStringLiteral(node.initializer)
      && visibleText(node.initializer.text)) {
      report(node.initializer, `hardcoded ${node.name.getText(file)} attribute`);
    }
    if (ts.isJsxAttribute(node) && localizedAttributes.has(node.name.getText(file))
      && node.initializer && ts.isJsxExpression(node.initializer)
      && node.initializer.expression && ts.isTemplateExpression(node.initializer.expression)) {
      const literalText = [
        node.initializer.expression.head.text,
        ...node.initializer.expression.templateSpans.map((span) => span.literal.text),
      ].join(" ");
      if (visibleText(literalText)) {
        report(node.initializer, `hardcoded dynamic ${node.name.getText(file)} attribute`);
      }
    }
    if (ts.isStringLiteral(node) && ts.isConditionalExpression(node.parent)
      && (node.parent.whenTrue === node || node.parent.whenFalse === node)
      && visibleText(node.text)) {
      let parent = node.parent;
      while (parent && !ts.isJsxExpression(parent) && !ts.isJsxAttribute(parent)) {
        parent = parent.parent;
      }
      const attribute = parent && ts.isJsxExpression(parent) && ts.isJsxAttribute(parent.parent)
        ? parent.parent
        : parent && ts.isJsxAttribute(parent) ? parent : null;
      if (parent && (!attribute || localizedAttributes.has(attribute.name.getText(file)))) {
        report(node, "hardcoded conditional UI text");
      }
    }
    ts.forEachChild(node, visit);
  };
  visit(file);
}

for (const file of readdirSync(localeDir).filter((name) => name.endsWith(".json"))) {
  const messages = JSON.parse(readFileSync(join(localeDir, file), "utf8"));
  if (requiredCompleteLocales.has(file)) {
    for (const key of sourceKeys) {
      if (!(key in messages) || !String(messages[key]).trim()) {
        console.error(`${file}: missing required key ${key}`);
        failed = true;
      }
    }
  }
  for (const key of Object.keys(messages)) {
    if (/MYMEMORY WARNING|TRANSLATION LIMIT|UNTRANSLATED/i.test(messages[key])) {
      console.error(`${file}: invalid translation service response for ${key}`);
      failed = true;
    }
    if (!sourceKeys.has(key)) {
      console.error(`${file}: unknown key ${key}`);
      failed = true;
    }
  }
  for (const key of Object.keys(messages)) {
    const variables = (value) => [...value.matchAll(variablePattern)].map((match) => match[1]).sort();
    if (JSON.stringify(variables(source[key])) !== JSON.stringify(variables(messages[key]))) {
      console.error(`${file}: variables differ for ${key}`);
      failed = true;
    }
  }
}

if (failed) process.exit(1);
console.log(`Checked ${sourceKeys.size} localization keys across ${readdirSync(localeDir).filter((name) => name.endsWith(".json")).length} locale bundles.`);
