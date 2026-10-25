import { createClient } from '@connectrpc/connect';
import { createGrpcWebTransport } from '@connectrpc/connect-web';
import { Operations } from './gen/operations_pb';
export const client = createClient(Operations, createGrpcWebTransport({baseUrl: new URL('./', document.baseURI).href}));
