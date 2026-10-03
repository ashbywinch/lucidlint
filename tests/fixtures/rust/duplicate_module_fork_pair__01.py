MODE = "prod"
TIMEOUT = 30


def connect():
    import socket
    return socket.socket()


def close(sock):
    return sock.close()
