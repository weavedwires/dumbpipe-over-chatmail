# Dumb pipe

Это пример использования [iroh](https://crates.io/crates/iroh) для создания "тупой трубы", соединяющей две машины по QUIC.

Iroh занимается hole punching и прохождением NAT там, где это возможно, а если hole punching не срабатывает — выполняет резервное соединение через релейный сервер.

Также полезна как отдельный инструмент для быстрого копирования данных.

Вдохновлено unix-утилитой [netcat](https://en.wikipedia.org/wiki/Netcat). Если netcat работает с IP-адресами, то dumbpipe — с 256-битными идентификаторами конечных точек, поэтому она в некоторой степени прозрачна к расположению. Кроме того, соединения шифруются с помощью TLS.

# Отличия от оригинала

Эта сборка привязана к [iroh](https://crates.io/crates/iroh) версии 0.35 и работает поверх встроенного в [chatmail](https://chatmail.io)-серверы iroh-релея как посредника: список релеев по умолчанию состоит из нескольких, работающих по всему миру, chatmail релеев. С официальными iroh-релеями, которые обслуживает n0, она **не** работает — они давно не поддерживают iroh версии 0.35.

Чтобы указать конкретный релей можно использовать ```-r nine.testrun.org```

Чтобы запретить подключаться в обход релеев (напрямую), например в целях конфиденциальности можно указать флаг ```--no-direct```

# Установка

Через [Cargo](https://doc.rust-lang.org/cargo/getting-started/installation.html):

```
cargo install dumbpipe
```

Если у вас установлен [Homebrew](https://brew.sh), можно установить так:

```
brew install dumbpipe
```

# Примеры

## Потоковое видео через dumbpipe с помощью [ffmpeg / ffplay](https://ffmpeg.org/):

Используются стандартный ввод и вывод.

### Сторона отправителя

На Mac OS:
```
ffmpeg -f avfoundation -r 30 -i "0" -pix_fmt yuv420p -f mpegts - | dumbpipe listen
```
На Linux:
```
ffmpeg -f v4l2 -i /dev/video0 -r 30 -preset ultrafast -vcodec libx264 -tune zerolatency -f mpegts - | dumbpipe listen
```
выводит тикет

### Сторона получателя
```
dumbpipe connect endpointealvvv4nwa522qhznqrblv6jxcrgnvpapvakxw5i6mwltmm6ps2r4aicamaakdu5wtjasadei2qdfuqjadakqk3t2ieq | ffplay -f mpegts -fflags nobuffer -framedrop -
```

- Подберите параметры ffmpeg под вашу платформу и устройства видеозахвата.
- Используйте тикет со стороны отправителя.

## Общий доступ к shell для парного программирования с помощью [tty-share](https://github.com/elisescu/tty-share):

Совместное использование терминальной сессии через интернет полезно при работе программистов в команде, но публичный [tty-share](https://github.com/elisescu/tty-share)-сервер не очень надёжен и, что важнее, [не шифруется сквозным шифрованием](https://tty-share.com/how-it-works/#end-to-end-encryption).

На сервере:

```
$ dumbpipe listen-tcp --host localhost:8000 &
$ tty-share
```

На клиенте(ах):

```
$ dumbpipe connect-tcp --addr localhost:8000 <ticket> &
$ tty-share http://localhost:8000/s/local/
```

## Проксирование веб-сервера для разработки

У вас есть dev-вебсервер на порту 3000, и вы хотите поделиться им с
коллегой из другого офиса или из другого конца света.

### Веб-сервер
```
npm run dev
>    - Local:        http://localhost:3000
```

### Слушатель dumbpipe

«Слушает» на конечной точке и перенаправляет все входящие запросы на dev-вебсервер, который слушает localhost на порту 3000. Через одну «трубу» может проходить любое количество соединений, но это будут отдельные локальные tcp-соединения.

```
dumbpipe listen-tcp --host localhost:3000
```
Эта команда выведет тикет, который можно использовать для подключения.

### Коннектор dumbpipe

«Слушает» на tcp-интерфейсе и порту на локальной машине. В данном случае — на порту 3001.
Перенаправляет все входящие соединения на конечную точку из тикета.

```
dumbpipe connect-tcp --addr 0.0.0.0:3001 <ticket>
```

### Проверка

Теперь вы можете открыть сайт на порту 3001.

## Проксирование приложения через Unix-сокет (например, Zellij)

Можно проксировать приложения, работающие через Unix-сокеты, например терминальный мультиплексор [Zellij](https://zellij.dev/).

Примечание: Zellij хранит свои сессионные сокеты в `$ZELLIJ_SOCKET_DIR/<VERSION>/session-name`

![image](https://github.com/user-attachments/assets/b8fbb988-57db-40cd-95e2-208e01fbaad6)

1. На удалённом хосте (с запущенным Zellij):

```bash
zellij --version
# zellij 0.42.2
# Прокидываем удалённый сокет Zellij
# Путь к сокету соответствует шаблону: /tmp/zellij-0/<VERSION>/<session-name>
dumbpipe listen-unix --socket-path /tmp/zellij-0/0.42.2/remote-task-1234
```

Это даст вам `<ticket>`.

2. На вашей локальной машине:

```bash
zellij --version
# zellij 0.42.1

# Создаём структуру каталогов для локального сокета
mkdir -p /tmp/zj-remote/0.42.1

# Создаём локальный сокет, подключённый к удалённому
dumbpipe connect-unix --socket-path /tmp/zj-remote/0.42.1/remote-task-1234 <ticket>
```

3. Подключаемся локальным клиентом Zellij:

```bash
# В новом окне/вкладке терминала укажите каталог сокетов и подключитесь
ZELLIJ_SOCKET_DIR=/tmp/zj-remote zellij attach remote-task-1234
```

# Расширенные возможности

## Комбинирование слушателей

Слушателей можно комбинировать. Например, перенаправить данные из удалённого Unix-сокета на локальный TCP-порт:

```bash
# Машина A: слушает на Unix-сокете
dumbpipe listen-unix --socket-path /var/run/my-app.sock

# Машина B: подключается через локальный TCP-порт
dumbpipe connect-tcp --addr 127.0.0.1:8080 <ticket>
```

## Свои ALPN

В dumbpipe есть экспертная возможность указать свой [ALPN](https://en.wikipedia.org/wiki/Application-Layer_Protocol_Negotiation) — строку. Её можно использовать для взаимодействия с существующими iroh-сервисами.

Например, вот как взаимодействовать с протоколом iroh-blobs:

```
echo request1.bin | dumbpipe connect <ticket> --custom-alpn utf8:/iroh-bytes/4 > response1.bin
```

(`/iroh-bytes/4` — это ALPN, используемый iroh-blobs 0.35. iroh-blobs — отдельный от iroh крейт; раньше он назывался iroh-bytes и поставлялся внутри крейта iroh.)

Если в request1.bin содержится корректный запрос для протокола `/iroh-bytes/4`, то в response1.bin теперь будет ответ.
