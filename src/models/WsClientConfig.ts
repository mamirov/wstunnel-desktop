export interface WsClientConfig {
    listenAddr: String,
    serverAddr: ServerAddr
}

export interface ServerAddr {
    scheme: String,
    host: String
}