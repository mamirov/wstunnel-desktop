<script lang="ts">
import {defineComponent, onMounted, ref} from 'vue'
import ClientOptionsEditor from "./ClientOptionsEditor.vue";
import {load, Store} from '@tauri-apps/plugin-store';
import {WsClientConfig} from "../models/WsClientConfig.ts";

const STORE_NAME = 'ws-client-config.json';

export default defineComponent({
  name: "Main",
  components: {ClientOptionsEditor},
  setup() {
    const mainContent = ref("Empty")
    const configList = ref<WsClientConfig[]>([]);
    const confToEdit = ref<WsClientConfig>();

    const loadConfig = async () => {
      const store = await load(STORE_NAME, {autoSave: false});
      const configs = await store.get<WsClientConfig[]>('ws-configs');
      if (configs === undefined) {
        await store.set<WsClientConfig[]>('ws-configs', []);
        await store.save();
      }
      configList.value = await store.get<WsClientConfig[]>('ws-configs');
    }

    const saveToStore = async () => {
      const store = await Store.get(STORE_NAME)
      await store?.set('ws-configs', configList.value);
      await store?.save();
    }

    const addNewConfig = async (clientConfig: WsClientConfig) => {
      configList.value?.push(clientConfig.value);
      await saveToStore();
      mainContent.value = '';
    }

    const openNewConfig = () => {
      mainContent.value = 'ClientOptionsEditor';
      confToEdit.value = undefined;
    }

    const pickConfig = (config: WsClientConfig) => {
      mainContent.value = 'ClientOptionsEditor';
      confToEdit.value = config;
    }

    const editConf = () => {
      console.log('Not implemented');
    }

    const deleteConf = async (config: WsClientConfig) => {
      configList.value = configList.value.filter(value => value.name !== config.name);
      await saveToStore();
    }


    onMounted(() => {
      loadConfig();
    })

    return {
      addNewConfig,
      confToEdit,
      configList,
      deleteConf,
      editConf,
      mainContent,
      openNewConfig,
      pickConfig
    }
  }
})
</script>

<template>
  <v-layout>

    <v-app-bar
        color="teal-darken-4">
      <v-app-bar-title>Wstunnel Client</v-app-bar-title>

      <v-spacer></v-spacer>

      <v-btn icon>
        <v-icon>mdi-text</v-icon>
        <v-tooltip
            activator="parent"
            location="bottom"
        >Logs
        </v-tooltip>
      </v-btn>

    </v-app-bar>

    <v-navigation-drawer permanent>
      <v-list>
        <v-list-item>
          <v-btn color="primary" @click="openNewConfig">Add new conf</v-btn>
        </v-list-item>
        <v-divider></v-divider>
        <v-list-item v-for="config in configList"
                     :key="config.name" @click="pickConfig(config)"
        >
          <v-layout class="mx-auto">
            {{ config.name }}
            <v-spacer></v-spacer>
            <v-btn icon size="small" @click="editConf(config)">
              <v-icon>mdi-pencil</v-icon>
              <v-tooltip
                  activator="parent"
                  location="bottom"
              >Edit
              </v-tooltip>
            </v-btn>
            <v-btn icon size="small" @click="deleteConf(config)">
              <v-icon>mdi-delete</v-icon>
              <v-tooltip
                  activator="parent"
                  location="bottom"
              >Delete
              </v-tooltip>
            </v-btn>
          </v-layout>
        </v-list-item>
      </v-list>
    </v-navigation-drawer>

    <v-main>
      <ClientOptionsEditor :key="confToEdit?.name"
                           v-if="mainContent === 'ClientOptionsEditor'"
                           :client-config="confToEdit"
                           @close="addNewConfig"/>
    </v-main>

  </v-layout>
</template>

<style scoped>
</style>