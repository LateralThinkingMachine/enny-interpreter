import assert from "node:assert/strict";
import test from "node:test";
import {speechHints,hasSpeechHints} from "../.test-build/speech-hints.js";

test("optional names preserve literal multilingual spelling and never infer from output language", () => {
  assert.deepEqual(speechHints("auto",""),{languages:["ja","en","hi"],keywords:[]});
  assert.equal(hasSpeechHints(speechHints("auto","")),false);
  assert.deepEqual(speechHints("ja","佐藤\nAC-42\nराजस्व"),{languages:["ja"],keywords:["佐藤","AC-42","राजस्व"]});
  assert.deepEqual(speechHints("hi","  परीक्षण  \r\n\r\nAPI"),{languages:["hi"],keywords:["परीक्षण","API"]});
  assert.equal(hasSpeechHints(speechHints("en","")),true);
});
test("hint limits reject oversize, duplicate and hidden control terms without truncation", () => {
  for (const terms of ["word\nword", "x".repeat(81), "字".repeat(81),Array.from({length:21},(_,i)=>`term${i}`).join("\n"),Array.from({length:20},(_,i)=>`${i}${"字".repeat(79)}`).join("\n"),"a\0b","a\tb","a\rb","<system>","a\u200bb","a\u202eb","a\u2066b", "\ud800", "\udfff", "\ufeffname", "name\ufeff"]) assert.throws(() => speechHints("auto",terms));
  assert.throws(() => speechHints("fr","bonjour"));
  const words = Array.from({length:20},(_,i)=>`${i}${"a".repeat(48)}`).join("\n");
  assert.equal(speechHints("en",words).keywords.length,20);
});
