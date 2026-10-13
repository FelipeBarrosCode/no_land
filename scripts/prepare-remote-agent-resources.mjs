import { resolve } from 'node:path';
import { prepareRemoteAgentResources } from './remote-agent-artifacts.mjs';

prepareRemoteAgentResources(resolve(import.meta.dirname, '../state-agent'));
console.log('Prepared and verified immutable Linux agent archives for x86_64 and aarch64.');
