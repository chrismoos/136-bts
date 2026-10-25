import { readFileSync, writeFileSync } from 'node:fs';
const path = 'src/gen/operations_pb.ts';
writeFileSync(path, readFileSync(path, 'utf8').trimEnd() + '\n');
