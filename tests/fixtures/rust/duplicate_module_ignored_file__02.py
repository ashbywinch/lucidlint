# lucidlint: ignore-file duplicate-module this fork is a deliberate port split
MODE = "prod"
TIMEOUT = 60


def connect():
    import socket
    return socket.socket()


def close(sock):
    return sock.close()
